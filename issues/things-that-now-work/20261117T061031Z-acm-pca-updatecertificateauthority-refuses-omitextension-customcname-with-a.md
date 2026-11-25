Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp
Issue-Shape-Id: com.amazonaws.acmpca#ACMPrivateCA
Issue-Key: acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp|run#28|AssertionError|assert refusal == (
Issue-Commit: 36b4752657a9c8fc6bd8a889740d640d6f7662fd

# acm-pca UpdateCertificateAuthority refuses OmitExtension+CustomCname with a paraphrased message instead of AWS's "You can't set a CustomCname if OmitExtension is set to true."

> _Note: this issue was identified against the code base at commit `36b4752657a9c8fc6bd8a889740d640d6f7662fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateCertificateAuthority

## Description
One cause.

Cause 1 of 1: the ValidationException message for the CrlConfiguration mutual exclusion between CrlDistributionPointExtensionConfiguration.OmitExtension=true and CustomCname is worded by the emulator itself, not as AWS words it. Failing check: acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp.

AWS: a CRL configuration that both sets CrlDistributionPointExtensionConfiguration.OmitExtension=true and names CustomCname is refused by request validation. Smithy (services/service-com-amazonaws-acmpca-acmprivateca/smithy-model/com-amazonaws-acmpca-acmprivateca.json): CrlConfiguration$CustomCname is the "Name inserted into the certificate CRL Distribution Points extension that enables the use of an alias for the CRL distribution point", and CrlConfiguration$CrlDistributionPointExtensionConfiguration "Configures the behavior of the CRL Distribution Point extension"; the constraint is NOT among UpdateCertificateAuthority's modelled errors (ConcurrentModificationException, InvalidArgsException, InvalidArnException, InvalidPolicyException, InvalidStateException, ResourceNotFoundException), so AWS refuses it with its generic request-validation rejection, HTTP 400 ValidationException. Recorded behavior: the fixed, whole-message text AWS returns for this error class is `You can't set a CustomCname if OmitExtension is set to true.` — it is entirely fixed text, with no part taken from the request (no bucket, CNAME, ARN or member path is interpolated).

Emulator: the status (400) and the error code (ValidationException) are right, but the message is the emulator's own paraphrase of the model's doc sentence: `RevocationConfiguration.CrlConfiguration.CrlDistributionPointExtensionConfiguration cannot be enabled with a custom CNAME set.` It is emitted as one fixed constant, so no request-derived part is involved; the whole sentence diverges.

Why: the error message is part of the contract for this error class. A client that matches on AWS's sentence (or asserts it in a test, surfaces it to an operator, or keys remediation off "CustomCname"/"OmitExtension" in the text) does not recognise the emulator's wording, so a correctly-refused request is reported as an unknown validation failure. Because the code is the generic ValidationException shared by every request-validation refusal of this operation, the message is the ONLY thing distinguishing this rule from another, which makes the divergence load-bearing rather than cosmetic.

Fix: the shared revocation-configuration validation that classifies this pair as a conflicting-members fault, and the single message constant that refusal carries. The same constant is read by CreateCertificateAuthority's validation, so correcting it there fixes both entry points to the rule; the surrounding classification (ValidationException, nothing written, prior configuration left intact) is already correct and must stay — the check's effect-absent assertions after the refusal pass.

Traces: acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp. Code citations: services/service-com-amazonaws-acmpca-acmprivateca/src/state.rs:1595-1597 (the message constant) and :1520-1529 (where it is raised as RevocationConfigurationFault::ConflictingMembers), lifted in services/service-com-amazonaws-acmpca-acmprivateca/src/ops/update_certificate_authority.rs:228-230.

Not covered by any open acm-pca issue: 20261025T011542Z-acm-pca-resource-policy-contract-diverges-putpolicy-rejection.md is about PutPolicy/DeletePolicy resource-policy diagnostics, and 20261114T135329Z-acm-pca-cloudformation-createpermission-accepts-an-unsupported-principal.md is about CreatePermission principals; neither touches CRL distribution-point validation wording.

## Evidence
Check acm-pca.crl-distribution-point-extension-configuration-governs-issued-cdp, Leg C. Request: UpdateCertificateAuthority(CertificateAuthorityArn=<an ACTIVE CA>, RevocationConfiguration={CrlConfiguration:{Enabled:true, ExpirationInDays:<n>, S3BucketName:<bucket>, S3ObjectAcl:'BUCKET_OWNER_FULL_CONTROL', CustomCname:<cname>, CrlDistributionPointExtensionConfiguration:{OmitExtension:true}}}).

Expected (AWS): HTTP 400, Error.Code='ValidationException', Error.Message='You can't set a CustomCname if OmitExtension is set to true.' (whole message is fixed text).

Actual (emulator): HTTP 400, Error.Code='ValidationException', Error.Message='RevocationConfiguration.CrlConfiguration.CrlDistributionPointExtensionConfiguration cannot be enabled with a custom CNAME set.'

Assertion: "OmitExtension 'cannot be enabled with a custom CNAME set', so the combination must be refused with ValidationException ... with AWS's exact message; got ('ValidationException', 'RevocationConfiguration.CrlConfiguration.CrlDistributionPointExtensionConfiguration cannot be enabled with a custom CNAME set.'). The immediately preceding OmitExtension-without-CNAME update on this same CA SUCCEEDED, so this is the combination being rejected and not the field being unsupported."

The refusal's status, code and side-effect-freedom all match; only the message text diverges.

## Steps to Reproduce
1. CreateCertificateAuthority (ROOT/RSA_2048/SHA256WITHRSA) and activate it.
2. Create an S3 bucket and grant PCA write access to it.
3. UpdateCertificateAuthority with RevocationConfiguration.CrlConfiguration {Enabled:true, ExpirationInDays, S3BucketName:<bucket>, S3ObjectAcl:'BUCKET_OWNER_FULL_CONTROL', CrlDistributionPointExtensionConfiguration:{OmitExtension:true}} — this succeeds, proving the field is supported.
4. Repeat the same update but ALSO pass CustomCname:<any name>.
5. Observe the ClientError: Code is ValidationException (correct) but Message is 'RevocationConfiguration.CrlConfiguration.CrlDistributionPointExtensionConfiguration cannot be enabled with a custom CNAME set.' instead of AWS's "You can't set a CustomCname if OmitExtension is set to true."
