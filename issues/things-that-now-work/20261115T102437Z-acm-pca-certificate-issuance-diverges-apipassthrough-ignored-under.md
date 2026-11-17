Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm-pca.api-passthrough-applies-only-under-a-passthrough-template acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp acm-pca.crl-next-update-derives-from-expiration-in-days acm-pca.issue-certificate-invalid-validity-period-rejected acm-pca.issued-notbefore-is-backdated-unless-validity-not-before-is-given acm-pca.ocsp-configuration-adds-an-authority-information-access-responder acm-pca.revoked-leaf-appears-in-audit-report-with-reason
Issue-Shape-Id: com.amazonaws.acmpca#ACMPrivateCA
Issue-Key: acm-pca.api-passthrough-applies-only-under-a-passthrough-template|run#23|AssertionError|assert pass_cns == [_API_CN], (
Issue-Key: acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp|run#17|AssertionError|assert default_uris, (
Issue-Key: acm-pca.crl-next-update-derives-from-expiration-in-days|_required_ext#34|AssertionError|raise AssertionError(
Issue-Key: acm-pca.issue-certificate-invalid-validity-period-rejected|run#5|AssertionError|assert refusal == ("ValidationException", _TOO_LONG_MESSAGE), refusal
Issue-Key: acm-pca.issued-notbefore-is-backdated-unless-validity-not-before-is-given|run#12|AssertionError|assert lo <= nb_a <= hi, (
Issue-Key: acm-pca.ocsp-configuration-adds-an-authority-information-access-responder|run#16|AssertionError|assert on_uris, (
Issue-Key: acm-pca.revoked-leaf-appears-in-audit-report-with-reason|run#26|AssertionError|assert "revokedAt" not in control_row and "revocationReason" not in control_row, (
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# ACM PCA certificate issuance diverges: ApiPassthrough ignored under the passthrough template, CDP/AIA/SKI extensions never emitted, notBefore not backdated, usage-mode validity refusal misclassified, and audit rows always carry revocation fields

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
IssueCertificate

## Description
Seven distinct root causes in the acm-pca emulator, all surfaced by behavioral checks of certificate issuance, CA certificate generation and the audit report. They share a service but are independent defects; each must be closed before submitting.

=== Cause 1 of 7: ApiPassthrough is ignored even under an *_APIPassthrough template variant ===
AWS: Smithy: IssueCertificate.ApiPassthrough -> ApiPassthrough{Extensions: Extensions, Subject: ASN1Subject}. Its documentation states "An APIPassthrough or APICSRPassthrough template variant must be selected, or else this parameter is ignored" and that when conflicting information arrives from several sources, PCA applies the documented order-of-operation rules (UsingTemplates.html#template-order-of-operations), under which API-supplied subject and extension values take priority over the CSR's. So with TemplateArn = arn:aws:acm-pca:::template/EndEntityCertificate_APIPassthrough/V1, the issued certificate's Subject RDNs (CN, O, ...) and its subjectAltName come from ApiPassthrough, while the identical request against arn:aws:acm-pca:::template/EndEntityCertificate/V1 signs the CSR's subject and SANs and discards ApiPassthrough.
Emulator: both legs of the check submitted a byte-identical ApiPassthrough and the same CSR, differing only in TemplateArn. The base-template leg behaved correctly (CSR subject signed), but the passthrough leg ALSO carried the CSR's CN and none of the API-supplied O= RDN or API-supplied DNS SAN: expected CN exactly ['api-subject.<domain>'], the issued certificate carried ['csr-subject.<domain>']. ApiPassthrough is dropped unconditionally, so the emulator treats the two template ARNs as interchangeable.
Why: a client that uses APIPassthrough to set subject, SANs or custom extensions at issuance time (the only way to do so without regenerating a CSR) silently receives certificates carrying CSR content instead. Nothing in the response signals the loss, and the wrong identity is cryptographically signed.
Fix: the IssueCertificate handler's template resolution and the certificate-assembly step that merges CSR-derived and API-supplied subject/extension values; the template ARN must select whether ApiPassthrough is applied or ignored.
Traces: acm-pca.api-passthrough-applies-only-under-a-passthrough-template

=== Cause 2 of 7: no CRL Distribution Points extension in certificates issued by a CRL-enabled CA ===
AWS: Smithy: CrlDistributionPointExtensionConfiguration.OmitExtension (@required boolean) is documented as "Configures whether the CRL Distribution Point extension should be populated with the default URL to the CRL. If set to true, then the CDP extension will not be present in any certificates issued by that CA unless otherwise specified through CSR or API passthrough." The converse is the default: with RevocationConfiguration.CrlConfiguration.Enabled = true and NO CrlDistributionPointExtensionConfiguration, every issued certificate carries the cRLDistributionPoints extension (OID 2.5.29.31) whose URI is the CA's default CRL URL, built from the CA's configured S3 bucket name and the signing region (or from CustomCname when one is set).
Emulator: with CRL generation enabled and no CrlDistributionPointExtensionConfiguration, the issued leaf carries no cRLDistributionPoints extension at all — the only extension OIDs present are ['2.5.29.19', '2.5.29.15', '2.5.29.37'] (basicConstraints, keyUsage, extendedKeyUsage); expected at least one URI naming the bucket the CA was configured with and the signing region.
Why: a relying party has no way to locate the CRL from the certificate, so revocation checking is impossible for every certificate this emulator issues; conversely a client cannot verify that OmitExtension=true actually took effect, because the extension is absent either way.
Fix: the extension-assembly step of the IssueCertificate handler, driven by the CA's RevocationConfiguration.CrlConfiguration (Enabled, S3BucketName, CustomCname, CrlDistributionPointExtensionConfiguration.OmitExtension).
Traces: acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp

=== Cause 3 of 7: the CA certificate carries no subjectKeyIdentifier extension ===
AWS: recorded behavior only (the Smithy model does not describe generated certificate contents). A PCA CA certificate carries a subjectKeyIdentifier extension (OID 2.5.29.14) derived from its public key, and the certificates and CRLs it signs carry a matching authorityKeyIdentifier; standard X.509 path building and CRL-to-issuer matching depend on it.
Emulator: the CA certificate carries only ['2.5.29.19', '2.5.29.15'] — basicConstraints and keyUsage. subjectKeyIdentifier is absent, so any client that resolves the issuer of a CRL or a leaf by key identifier fails before it can evaluate anything else.
Why: chain building against the emulator's CA fails for conformant clients, and revocation artifacts cannot be bound to the issuing key.
Fix: the CA certificate generation path (the self-signed root / imported-CA certificate construction in the acm-pca state and signing component), and the corresponding authorityKeyIdentifier on signed certificates and CRLs.
Traces: acm-pca.crl-next-update-derives-from-expiration-in-days

=== Cause 4 of 7: validity-exceeds-usage-mode refusal uses the wrong error code and message ===
AWS: recorded behavior only for the code — the acm-pca Smithy model does not define a ValidationException shape at all (IssueCertificate models InvalidArgsException, InvalidArnException, InvalidStateException, LimitExceededException, MalformedCSRException, ResourceNotFoundException). The recording shows that IssueCertificate against a SHORT_LIVED_CERTIFICATE CA with Validity={Type: DAYS, Value: 30} is refused with HTTP 400, Error.Code "ValidationException" and Error.Message "The requested certificate validity exceeds the maximum allowed by the usage mode. Reduce the requested validity period to be equal to or less than that supported by the usage mode for this CA or use a CA with a different usage mode." That message is entirely fixed text — it interpolates no request value, no limit and no member name — and the refusal is deterministic (repeating the call fails identically) while a validity at the 7-day boundary succeeds.
Emulator: the call is refused, but with Error.Code "InvalidArgsException" and Error.Message "Validity may not exceed seven days for a certificate authority whose UsageMode is SHORT_LIVED_CERTIFICATE" — a code AWS does not use for this class and a message that shares no text with AWS's.
Why: a client that branches on Error.Code (or matches the documented message) to tell "shorten the validity" from a generic bad-argument error takes the wrong branch; SDK error-shape matching for ValidationException never fires.
Fix: the IssueCertificate handler's validity validation — the error it constructs for the usage-mode maximum (code and message), not the detection itself, which already fires at the right point and before any certificate is minted.
Traces: acm-pca.issue-certificate-invalid-validity-period-rejected

=== Cause 5 of 7: notBefore is not backdated when ValidityNotBefore is omitted ===
AWS: Smithy: IssueCertificate.ValidityNotBefore is optional and "specifies a customized starting time for the validity period". Recorded behavior for the default: when it is omitted, PCA sets the certificate's notBefore to the issuance instant minus 60 minutes (3600s), so the certificate is immediately usable by relying parties whose clocks lag. The requirement is the 3600s offset from the issuance instant, not any absolute time.
Emulator: notBefore equals the issuance instant. IssueCertificate was called between T and T+2s and the issued certificate carries notBefore = T truncated to the second, where it must lie 3600s earlier.
Why: every certificate the emulator issues is unusable on any relying party whose clock is even slightly behind, and code that asserts the one-hour skew window (common for short-lived certificates, whose whole validity is days) sees a different validity period than production.
Fix: the notBefore computation in the IssueCertificate handler: default to issuance-instant minus 3600s, and use ValidityNotBefore verbatim when supplied.
Traces: acm-pca.issued-notbefore-is-backdated-unless-validity-not-before-is-given

=== Cause 6 of 7: no authorityInformationAccess OCSP responder in certificates issued by an OCSP-enabled CA ===
AWS: Smithy: RevocationConfiguration.OcspConfiguration{Enabled @required, OcspCustomCname}; OcspCustomCname is documented as "By default, Amazon Web Services Private CA injects an Amazon Web Services domain into certificates being validated by the Online Certificate Status Protocol (OCSP). A customer can alternatively use this [custom CNAME]". So with OcspConfiguration.Enabled = true, every issued certificate carries an authorityInformationAccess extension (OID 1.3.6.1.5.5.7.1) containing an accessMethod id-ad-ocsp accessDescription whose URI names the configured OcspCustomCname, or the AWS-injected OCSP domain when none is configured.
Emulator: with OcspConfiguration.Enabled=true and an OcspCustomCname configured on the CA, the issued certificate carries no authorityInformationAccess extension — the extension OIDs present are only ['2.5.29.19', '2.5.29.15', '2.5.29.37'], with no id-ad-ocsp URI containing the configured responder host.
Why: OCSP revocation checking is unavailable for every certificate issued, and a client cannot verify that enabling OCSP (or pointing it at a custom responder) had any effect.
Fix: the same extension-assembly step of IssueCertificate, driven by the CA's RevocationConfiguration.OcspConfiguration.
Traces: acm-pca.ocsp-configuration-adds-an-authority-information-access-responder

=== Cause 7 of 7: audit-report rows always carry revokedAt and revocationReason, even for certificates that were never revoked ===
AWS: recorded behavior, consistent with the audit-report documentation (privateca userguide PcaAuditReport.html): a report row records revocation state ONLY for a certificate that has been revoked. The members revokedAt and revocationReason are ABSENT from the row of a certificate that was never revoked; a revoked row carries revokedAt (>= its issuedAt) and the revocationReason RevokeCertificate was given.
Emulator: the revoked certificate's row is correct (revocationReason and a revokedAt >= issuedAt both present and right), but the row of an untouched leaf issued by the same CA also contains both members, emitted as empty strings: {..., 'revokedAt': '', 'revocationReason': '', ...}. Presence is the signal AWS uses, so the emulator's report reports revocation state for every certificate it has ever issued.
Why: a consumer that tests for the presence of revokedAt/revocationReason (the documented way to read the report) classifies every certificate as revoked; one that parses the values gets an empty string where a timestamp or an enum is expected and fails to parse.
Fix: the audit-report generation/serialization component behind CreateCertificateAuthorityAuditReport — omit the revocation members for rows whose certificate has no revocation record instead of emitting empty placeholders.
Traces: acm-pca.revoked-leaf-appears-in-audit-report-with-reason

Note on scope: the two other acm-pca failures in this run (acm-pca.ca-resource-policy-lockout-guard-classified and acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected) are already owned by open issues 20261025T011542Z-acm-pca-resource-policy-contract-diverges-putpolicy-rejection.md and 20261114T135329Z-acm-pca-cloudformation-createpermission-accepts-an-unsupported-principal.md and are not part of this issue. No existing open issue covers any of the seven causes above.

## Evidence
Behavioral suite: 119/128 passed; 9 mismatches, all under the acm-pca / acm-pca__cloudformation headings. The seven causes above, with the replayed assertion failures verbatim:

Cause 1 — acm-pca.api-passthrough-applies-only-under-a-passthrough-template:
  AssertionError: under arn:aws:acm-pca:::template/EndEntityCertificate_APIPassthrough/V1 the API-supplied Subject takes priority over the CSR's: expected CN exactly ['api-subject.sim2real.invalid'], the issued certificate carries ['csr-subject.sim2real.invalid'] (the CSR asked for 'csr-subject.sim2real.invalid')
  (the base-template leg of the same check passed, so ApiPassthrough is dropped under BOTH template ARNs)

Cause 2 — acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp:
  AssertionError: with CRL generation enabled and no CrlDistributionPointExtensionConfiguration, the docs promise the CDP extension 'will be present and contain the default CRL URL': expected at least one URI naming bucket '<run bucket>', the certificate carries [] (all extension OIDs present: ['2.5.29.19', '2.5.29.15', '2.5.29.37'])
  Expected: one cRLDistributionPoints (2.5.29.31) URI naming the CA's configured S3 bucket and the signing region. Actual: extension absent.

Cause 3 — acm-pca.crl-next-update-derives-from-expiration-in-days:
  AssertionError: the CA certificate must carry a subjectKeyIdentifier extension, but the extension is absent; the OIDs actually present are ['2.5.29.19', '2.5.29.15']
  Expected: subjectKeyIdentifier (2.5.29.14) on the CA certificate. Actual: only basicConstraints and keyUsage.

Cause 4 — acm-pca.issue-certificate-invalid-validity-period-rejected:
  Expected (recording): ('ValidationException', 'The requested certificate validity exceeds the maximum allowed by the usage mode. Reduce the requested validity period to be equal to or less than that supported by the usage mode for this CA or use a CA with a different usage mode.') — fixed text, no request-derived values.
  Actual (emulator): ('InvalidArgsException', 'Validity may not exceed seven days for a certificate authority whose UsageMode is SHORT_LIVED_CERTIFICATE')
  Both at HTTP 400; the call is refused either way, only the classification and wording diverge.

Cause 5 — acm-pca.issued-notbefore-is-backdated-unless-validity-not-before-is-given:
  AssertionError: with no ValidityNotBefore, notBefore must be the issuance instant minus 3600s: IssueCertificate was called between 2026-11-15 10:17:48.901925+00:00 and 2026-11-15 10:17:50.921495+00:00, so notBefore must lie in [2026-11-15 09:15:48.901925+00:00, 2026-11-15 09:19:50.921495+00:00], but the issued certificate carries 2026-11-15 10:17:48+00:00
  (the timestamps are this run's; the requirement is the -3600s offset from the issuance instant)

Cause 6 — acm-pca.ocsp-configuration-adds-an-authority-information-access-responder:
  AssertionError: with OcspConfiguration.Enabled=True the CA must place an OCSP responder URL in the authorityInformationAccess extension of the certificates it issues: expected at least one id-ad-ocsp URI containing '<configured OcspCustomCname>', the certificate carries [] (all extension OIDs present: ['2.5.29.19', '2.5.29.15', '2.5.29.37'])

Cause 7 — acm-pca.revoked-leaf-appears-in-audit-report-with-reason:
  AssertionError: the leaf that was never revoked carries revocation state in its audit row: {'awsAccountId': '...', 'certificateArn': '...', 'serial': '...', 'subject': 'CN=sim2real-control', 'notBefore': '...', 'notAfter': '...', 'issuedAt': '...', 'revokedAt': '', 'revocationReason': '', 'templateArn': 'arn:aws:acm-pca:::template/EndEntityCertificate/V1'}
  Expected: the unrevoked row OMITS revokedAt and revocationReason entirely. Actual: both members present as empty strings. The revoked serial's own row asserted correct earlier in the same check, so only the unrevoked rows are wrong.

## Steps to Reproduce
All steps use the acm-pca client against the emulator. Each leg creates a CA, waits for ACTIVE, and tears it down (DISABLED then DeleteCertificateAuthority with PermanentDeletionTimeInDays=7).

Cause 1: create and activate a root CA. Build one CSR with CN=csr-subject.<domain> and a DNS SAN csr-san.<domain>. Call IssueCertificate twice with the SAME Csr and the SAME ApiPassthrough={Subject:{CommonName:'api-subject.<domain>', Organization:'<org>'}, Extensions:{SubjectAlternativeNames:[{DnsName:'api-san.<domain>'}]}}, once with TemplateArn=arn:aws:acm-pca:::template/EndEntityCertificate/V1 and once with .../EndEntityCertificate_APIPassthrough/V1. GetCertificate both and parse. The passthrough leg must carry CN=api-subject.<domain>, O=<org> and the api-san DNS SAN; the emulator returns the CSR's subject/SAN on both legs.

Cause 2: create two CAs sharing one S3 bucket; enable CRL on both via UpdateCertificateAuthority RevocationConfiguration.CrlConfiguration{Enabled:true, S3BucketName:<bucket>, ExpirationInDays:N}, one with no CrlDistributionPointExtensionConfiguration and one with {OmitExtension:true}. Issue a leaf from each with the same CSR and inspect extensions. The default-configuration leaf must carry 2.5.29.31 with a URI naming <bucket> and the signing region; the emulator emits no 2.5.29.31.

Cause 3: create and activate a root CA, then GetCertificateAuthorityCertificate and parse the CA certificate's extensions. subjectKeyIdentifier (2.5.29.14) must be present; the emulator returns only 2.5.29.19 and 2.5.29.15.

Cause 4: create and activate a root CA with UsageMode=SHORT_LIVED_CERTIFICATE. Call IssueCertificate with Validity={Type:'DAYS', Value:30}. Inspect Error.Code and Error.Message: expected ValidationException with the usage-mode message above; the emulator answers InvalidArgsException with its own wording. Repeat to confirm determinism, then issue with Validity={Type:'DAYS', Value:7} to confirm the boundary still succeeds.

Cause 5: create and activate a root CA. Record the UTC instant before and after an IssueCertificate call made WITHOUT ValidityNotBefore. GetCertificate and read notBefore: it must be ~3600s before the issuance instant; the emulator returns the issuance instant itself.

Cause 6: create and activate a root CA, then UpdateCertificateAuthority with RevocationConfiguration.OcspConfiguration={Enabled:true, OcspCustomCname:'<host>'}. Issue a leaf and inspect extensions: authorityInformationAccess (1.3.6.1.5.5.7.1) with an id-ad-ocsp URI containing <host> must be present; the emulator emits no AIA extension.

Cause 7: create and activate a root CA, issue two leaves (one CN=sim2real-control, one to be revoked), RevokeCertificate the second with a RevocationReason, then CreateCertificateAuthorityAuditReport, poll DescribeCertificateAuthorityAuditReport until the report is ready and read it from S3. The revoked serial's row must carry revokedAt and revocationReason; the control serial's row must contain NEITHER key. The emulator emits both keys with empty-string values on the control row.
