Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected
Issue-Key: acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected|run#14|AssertionError|assert status == "CREATE_FAILED", f"rejected stack ended {status!r}"
Issue-Commit: 743af1aef2870b34c6c9092ae87b31cfc860bafb

# acm-pca__cloudformation: CreatePermission accepts an unsupported Principal, so a CFN AWS::ACMPCA::Permission with a bad principal reaches CREATE_COMPLETE and grants the permission

> _Note: this issue was identified against the code base at commit `743af1aef2870b34c6c9092ae87b31cfc860bafb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePermission

## Description
Cause 1 of 1: ACM-PCA CreatePermission does not validate that Principal is a service principal ACM Private CA supports.

AWS: CreatePermission only grants to a supported service principal (in practice acm.amazonaws.com). Any other value is refused before anything is granted, with HTTP 400 and error code ValidationException whose message is the fixed sentence "The principal specified is not supported." — relayed by CloudFormation into the resource's CREATE_FAILED reason as "<message> (Service: AWSACMPCA; Status Code: 400; Error Code: ValidationException; Request ID: <id>; Proxy: null)" with HandlerErrorCode GeneralServiceException. The fixed text is the whole sentence and the Service/Status Code/Error Code tokens; only the Request ID is per-request. Smithy (services/service-com-amazonaws-acmpca-acmprivateca): CreatePermission.Principal is @required, @length 0-128, @pattern ^[^*]+$ — the model constrains only the shape, so the supported-principal rule is a service-side check; its status, code and wording are recorded behavior.

Emulator: the same call is accepted. The stack declaring AWS::ACMPCA::Permission with Principal "not-acm.amazonaws.com" (Actions IssueCertificate/GetCertificate/ListPermissions, CertificateAuthorityArn the stack's own CA) reaches CREATE_COMPLETE — no CREATE_FAILED event, no refusal — and the permission is granted on the CA (ListPermissions then reports it). The positive control with acm.amazonaws.com behaves correctly, so the divergence is purely the missing principal check.

Why: a client that relies on ACM-PCA to reject an unsupported principal gets a CA whose resource policy grants a principal AWS would never have accepted, and a CloudFormation deployment that AWS fails fast is reported as a success. Negative tests of this guard pass vacuously, and infrastructure that is invalid in AWS is modelled as valid here.

Fix: the CreatePermission handler's input validation in the acm-pca service (service-com-amazonaws-acmpca-acmprivateca) must reject an unsupported Principal with 400 ValidationException and the message above, before any permission is recorded in state; the CloudFormation AWS::ACMPCA::Permission resource handler must then surface that refusal as the resource's CREATE_FAILED reason (GeneralServiceException).

Traces: behavioral check acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected — "AssertionError: rejected stack ended 'CREATE_COMPLETE'".

## Evidence
Check: acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected [mismatch]

Expected (AWS, as recorded): the stack whose AWS::ACMPCA::Permission has Principal="not-acm.amazonaws.com" ends CREATE_FAILED, the CaPermission CREATE_FAILED reason is ("The principal specified is not supported. (Service: AWSACMPCA; Status Code: 400; Error Code: ValidationException; Request ID: <id>; Proxy: null)", "GeneralServiceException"), and ListPermissions on that CA returns [].

Actual (emulator): stack status 'CREATE_COMPLETE'; no CREATE_FAILED row for CaPermission; the permission is granted.

Positive control in the same check (Principal="acm.amazonaws.com") reaches CREATE_COMPLETE with exactly one permission listing the templated principal and actions — that part passes.

## Steps to Reproduce
1. Create a stack with two resources: an AWS::ACMPCA::CertificateAuthority (Type ROOT, RSA_2048/SHA256WITHRSA, UsageMode SHORT_LIVED_CERTIFICATE) and an AWS::ACMPCA::Permission with CertificateAuthorityArn={"Ref":"Ca"}, Actions [IssueCertificate, GetCertificate, ListPermissions] and Principal "not-acm.amazonaws.com"; pass DisableRollback=True.
2. Poll DescribeStacks until terminal. Emulator: CREATE_COMPLETE. AWS: CREATE_FAILED with the CaPermission reason above.
3. Call acm-pca ListPermissions with CertificateAuthorityArn of the stack's CA. Emulator: one permission for not-acm.amazonaws.com. AWS: [].
4. Equivalently, call acm-pca CreatePermission directly with Principal="not-acm.amazonaws.com": emulator answers success, AWS answers 400 ValidationException "The principal specified is not supported."
