Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift-serverless__secretsmanager.namespace-managed-admin-password-yields-describable-secret redshift-serverless__secretsmanager.delete-secret-refuses-namespace-managed-admin-password-secret redshift-serverless__secretsmanager.namespace-create-rejects-combined-manage-and-explicit-admin-password redshift-serverless__secretsmanager.namespace-update-manage-admin-password-switches-from-explicit-to-managed
Issue-Key: redshift-serverless__secretsmanager.namespace-managed-admin-password-yields-describable-secret|run#10|AssertionError|assert secret_arn, (
Issue-Key: redshift-serverless__secretsmanager.delete-secret-refuses-namespace-managed-admin-password-secret|run#9|AssertionError|assert managed_secret_arn, (
Issue-Key: redshift-serverless__secretsmanager|CreateNamespace|ValidationException
Issue-Key: redshift-serverless|UpdateNamespace|ValidationException
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# redshift-serverless__secretsmanager: 3 net-new causes — manageAdminPassword creates no Secrets Manager secret (no adminPasswordSecretArn), the combined-password ValidationException is emulator-worded, and UpdateNamespace refuses the switch to a managed password

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNamespace

## Description
Three independent root causes in redshift-serverless's managed-admin-password integration with Secrets Manager.

Cause 1 of 3: a namespace created with manageAdminPassword=true never gets a managed admin-password secret, so the namespace carries no adminPasswordSecretArn.
AWS: Smithy: CreateNamespace accepts `manageAdminPassword` (Boolean) and `adminPasswordSecretKmsKeyId`, and the Namespace output shape declares `adminPasswordSecretArn` — 'The Amazon Resource Name (ARN) for the namespace's admin user credentials secret' — and `adminPasswordSecretKmsKeyId`. Recorded behavior: a namespace created with manageAdminPassword=true and no adminUserPassword reaches status AVAILABLE carrying adminPasswordSecretArn, that ARN resolves in Secrets Manager via DescribeSecret, the described secret carries OwningService 'redshift', and an ordinary secret created by the same caller carries NO OwningService.
Emulator: GetNamespace on the AVAILABLE namespace returns a Namespace with adminUsername, creationDate, dbName, iamRoles, kmsKeyId, lakehouseRegistrationStatus, logExports, namespaceArn, namespaceId, namespaceName and status — and no adminPasswordSecretArn member at all. No secret is created in Secrets Manager, so nothing downstream of it exists either: DescribeSecret has no secret to resolve, there is no secret carrying OwningService 'redshift', and DeleteSecret can never be exercised against a service-owned secret.
Why: a client that enables managed admin passwords has no way to find the credentials it was promised — the only handle to them (adminPasswordSecretArn) is absent, so password retrieval, rotation and any IAM/KMS policy attached to that secret are all unreachable. Cross-service behavior built on secret ownership (Secrets Manager refusing writes to a Redshift-owned secret) also cannot occur.
Fix: the CreateNamespace handler and the namespace state/serializer in the redshift-serverless service — on manageAdminPassword=true it must provision the backing Secrets Manager secret (marked as owned by the `redshift` service) and project its ARN into Namespace.adminPasswordSecretArn for CreateNamespace, GetNamespace and UpdateNamespace alike.
Traces: redshift-serverless__secretsmanager.namespace-managed-admin-password-yields-describable-secret, redshift-serverless__secretsmanager.delete-secret-refuses-namespace-managed-admin-password-secret.

Cause 2 of 3: CreateNamespace's refusal of manageAdminPassword=true together with an explicit adminUserPassword uses the emulator's own sentence.
AWS: Smithy: CreateNamespace models ValidationException (HTTP 400) and declares both `adminUserPassword` (DbPassword, sensitive) and `manageAdminPassword` (Boolean); supplying both is invalid. Recorded behavior: the call is refused with code ValidationException and the message 'The AdminUserPassword parameter cannot be provided if ManagedAdminPassword is true.' — entirely fixed text naming the two parameters in AWS's API-parameter capitalization (AdminUserPassword, ManagedAdminPassword), with no values from the request in it.
Emulator: the code is right (ValidationException) but the message is 'adminUserPassword cannot be set when manageAdminPassword is true' — different wording, wire-member capitalization, no trailing period.
Why: a client that matches the refusal text to tell this conflict apart from the other ValidationException causes CreateNamespace returns misclassifies it, and a user is shown a sentence AWS never emits.
Fix: the CreateNamespace handler's request validation in the redshift-serverless service — the message template only.
Traces: redshift-serverless__secretsmanager.namespace-create-rejects-combined-manage-and-explicit-admin-password.

Cause 3 of 3: UpdateNamespace refuses the legal switch from an explicit admin password to a managed one.
AWS: Smithy: UpdateNamespace accepts `namespaceName` (required), `adminUsername`, `adminUserPassword`, `manageAdminPassword` and `adminPasswordSecretKmsKeyId`, and returns a required `namespace`. Recorded behavior: on a namespace created with adminUsername + an explicit adminUserPassword (which carries no adminPasswordSecretArn), UpdateNamespace(namespaceName=<ns>, adminUsername=<same user>, manageAdminPassword=true) is ACCEPTED; the namespace settles AVAILABLE carrying an adminPasswordSecretArn that a fresh GetNamespace agrees with and that DescribeSecret resolves with no DeletedDate. The 'update the username and password together' rule applies to supplying an explicit adminUserPassword, not to handing password management to the service.
Emulator: the call is refused with HTTP 400 ValidationException 'adminUsername and adminUserPassword must be updated together'. The emulator applies its pairing rule to manageAdminPassword=true as though a password had been omitted, so the transition from an explicit password to a managed one is impossible.
Why: a client migrating an existing namespace onto managed credentials is blocked outright, and the refusal blames a parameter the caller deliberately did not send. Downstream, no secret is created and no adminPasswordSecretArn ever appears.
Fix: the UpdateNamespace handler's validation in the redshift-serverless service — the adminUsername/adminUserPassword pairing check must not fire when manageAdminPassword=true — plus the same secret provisioning and adminPasswordSecretArn projection described in Cause 1.
Traces: redshift-serverless__secretsmanager.namespace-update-manage-admin-password-switches-from-explicit-to-managed.

## Evidence
Cause 1 — expected: GetNamespace(...)['namespace']['adminPasswordSecretArn'] present on an AVAILABLE manageAdminPassword=true namespace, resolvable by secretsmanager DescribeSecret with OwningService == 'redshift'. Actual: AssertionError: an AVAILABLE managed-password namespace must carry adminPasswordSecretArn, got {'adminUsername': 'admin1', 'dbName': 'dev', 'iamRoles': [], 'kmsKeyId': 'AWS_OWNED_KMS_KEY', 'lakehouseRegistrationStatus': 'NOT_REGISTERED', 'logExports': [], 'namespaceArn': 'arn:aws:redshift-serverless:<region>:<account>:namespace/<id>', 'namespaceId': '<id>', 'namespaceName': '<ns>', 'status': 'AVAILABLE'} — the member is absent, not empty. (The ids/timestamps in the replayed dict are per-run values and not part of the requirement.)

Cause 2 — expected ('ValidationException', 'The AdminUserPassword parameter cannot be provided if ManagedAdminPassword is true.'); actual ('ValidationException', 'adminUserPassword cannot be set when manageAdminPassword is true').

Cause 3 — expected: UpdateNamespace 200 whose namespace settles AVAILABLE with an adminPasswordSecretArn. Actual: botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the UpdateNamespace operation: adminUsername and adminUserPassword must be updated together (HTTP 400).

## Steps to Reproduce
Cause 1: CreateNamespace(namespaceName=<ns>, adminUsername='admin1', manageAdminPassword=True); poll GetNamespace(namespaceName=<ns>) until status AVAILABLE; the returned namespace has no adminPasswordSecretArn, and no corresponding secret exists in Secrets Manager (so DescribeSecret/DeleteSecret on a Redshift-owned secret cannot be exercised).

Cause 2: CreateNamespace(namespaceName=<ns2>, adminUsername='admin1', adminUserPassword='<a valid password>', manageAdminPassword=True) — refused with ValidationException but with the wrong message.

Cause 3: CreateNamespace(namespaceName=<ns3>, adminUsername='admin1', adminUserPassword='<a valid password>'); wait AVAILABLE (no adminPasswordSecretArn, as expected); then UpdateNamespace(namespaceName=<ns3>, adminUsername='admin1', manageAdminPassword=True) — refused with ValidationException 'adminUsername and adminUserPassword must be updated together' instead of succeeding.
