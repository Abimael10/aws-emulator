Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__ecr__kms.a-kms-encrypted-repository-is-audited-as-ecr-acting-on-the-callers-behalf
Issue-Key: cloudtrail__ecr__kms.a-kms-encrypted-repository-is-audited-as-ecr-acting-on-the-callers-behalf|_await#23|Timeout|time.sleep(_POLL_INTERVAL_S)
Issue-Commit: 4e953d0ceb021d373a4669be7884bbadfda507a7

# cloudtrail__ecr__kms: a KMS-encrypted ECR repository mints no KMS grant on the customer CMK, so the ECR/KMS/CloudTrail attribution check is cut off at its 60 s cap

> _Note: this issue was identified against the code base at commit `4e953d0ceb021d373a4669be7884bbadfda507a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
One net-new root cause, grounded in the single failing check of this service group.

Cause 1 of 1: ECR's CreateRepository with encryptionConfiguration={encryptionType: KMS, kmsKey: <CMK>} has no KMS-side effect — no grant is created on the named customer-managed key, so kms:ListGrants for that key stays empty forever and every consumer that waits for ECR's grant blocks until its own deadline. Failing check: cloudtrail__ecr__kms.a-kms-encrypted-repository-is-audited-as-ecr-acting-on-the-callers-behalf.

AWS: Smithy (com.amazonaws.ecr): CreateRepositoryRequest.encryptionConfiguration targets EncryptionConfiguration (encryptionType AES256|KMS|KMS_DSSE, kmsKey) and CreateRepository declares KmsException (client error, HTTP 400); DeleteRepository likewise declares KmsException — both operations are modelled as doing real work against KMS, not as storing an opaque blob. The model cannot express the grant itself, so for the grant this is recorded behavior only: on real AWS, creating a repository with a customer CMK makes ECR take a grant on that key that becomes visible through kms:ListGrants within seconds, with GranteePrincipal naming an ECR service principal and Operations including at least Decrypt and GenerateDataKey (the check asserts that subset, since the full operation set is undocumented); deleting the repository retires that grant, and an AES256 repository takes none. The same recording shows the two CloudTrail legs that follow from it: the caller's own ecr.amazonaws.com CreateRepository record attributed to the caller's identity with NO userIdentity.invokedBy, and the KMS calls ECR makes on the caller's behalf recorded with eventSource kms.amazonaws.com, the caller's userIdentity.type/arn and userIdentity.invokedBy == "ecr.amazonaws.com", carrying no key material.

Emulator: CreateRepository answers 200 and echoes encryptionConfiguration back (the check's assertions on encryptionType == "KMS" and kmsKey == the key ARN are reached and do not fail), but the first unbounded wait after it — the poll of kms:ListGrants for an ECR grant on the CMK, budget 300 s, interval 2 s — never satisfies its predicate and the harness kills the check with "Check timed out after 60.0s". Every call before that poll is a fast control-plane call (sts:GetCallerIdentity, kms:CreateKey, kms:PutKeyPolicy, a baseline kms:ListGrants that correctly returns no ECR grant, ecr:CreateRepository), so the whole budget is spent waiting for a grant the emulator never mints. Because the check dies inside that poll, the downstream legs (the CloudTrail CreateRepository record, the invokedBy=ecr.amazonaws.com KMS record, the grant retirement on DeleteRepository, and the RepositoryNotFoundException message "The repository with name '<repositoryName>' does not exist in the registry with id '<accountId>'" — fixed text with repository name and registry account id taken from the request/state) are never exercised and remain unverified behind this one cause.

Why: a client is told its repository is encrypted with its own key while nothing in KMS records that authorization. A key-usage audit (kms:ListGrants, key-policy review, CloudTrail) concludes no service holds a grant on the CMK; revoking or rotating the key has no observable relationship to the repository; a teardown reconciler cannot distinguish "ECR retired its grant" from "ECR never took one"; and any flow that waits for the grant before treating the repository as usable hangs.

Fix: the ECR CreateRepository handler's encryptionConfiguration path must, for encryptionType KMS and KMS_DSSE, create the grant in the emulator's KMS state against the named CMK (grantee an ECR service principal, Operations including Decrypt and GenerateDataKey, encryption-context constraint naming the repository), and the DeleteRepository handler must retire it; encryptionType AES256 must create none. The audit side belongs with it: the ECR/KMS cross-service call path must emit CloudTrail records for the KMS calls ECR makes on the caller's behalf with the caller's userIdentity plus invokedBy "ecr.amazonaws.com", while the caller's own CreateRepository record must carry no invokedBy.

Related open issue, which does NOT cover this check: 20261024T182018Z-ecr-kms-ecr-createrepository-with-encryptiontype-kms-creates-no-kms.md describes the same missing grant from a different corpus and carries a different replay key (ecr__kms.kms-encrypted-repository-creates-exactly-one-ecr-grant-and-retires-it-on-delete); this check additionally gates the CloudTrail attribution legs behind it. Fixing either should be validated against both replay keys.

Traces: cloudtrail__ecr__kms.a-kms-encrypted-repository-is-audited-as-ecr-acting-on-the-callers-behalf.

## Evidence
Cause 1 — verdict [mismatch], detail "Check timed out after 60.0s" for check cloudtrail__ecr__kms.a-kms-encrypted-repository-is-audited-as-ecr-acting-on-the-callers-behalf.

Expected (recorded AWS behavior): after ecr:CreateRepository with encryptionConfiguration={'encryptionType':'KMS','kmsKey':<CMK ARN>}, kms:ListGrants on that key returns >= 1 grant whose GranteePrincipal/Name identifies ECR and whose Operations include both Decrypt and GenerateDataKey; ecr:DeleteRepository retires that GrantId and the listing converges back to carrying none.

Actual (emulator): kms:ListGrants on the CMK returns no ECR grant for the whole 300 s poll budget; the harness cap fires first and the check is reported as timed out. The check's own failure text for this leg is "DATAPLANE_UNIMPLEMENTED: ECR must take a KMS grant on the CMK backing a KMS-encrypted repository: expected >=1 ECR grant on <keyId> within 300s, actual 0 of <n> grants total". The preceding assertions pass, which localises the divergence: the baseline kms:ListGrants correctly shows no ECR grant, and CreateRepository returns 200 with encryptionConfiguration.encryptionType == 'KMS' and kmsKey == the key ARN echoed back. So the request is accepted and stored but produces no KMS-side effect.

Blocked behind the same cause (not independently observed, listed so a fixer re-runs them): the CloudTrail CreateRepository record (eventSource ecr.amazonaws.com, caller userIdentity, no invokedBy, requestParameters naming the CMK), the KMS record with userIdentity.invokedBy == 'ecr.amazonaws.com' and no key material, the grant retirement on delete, and DeleteRepository/DescribeRepositories' RepositoryNotFoundException message.

## Steps to Reproduce
Against the emulator's ECR, KMS and CloudTrail endpoints in one region, as one account:
1. key = kms.create_key(KeyUsage='ENCRYPT_DECRYPT'); note KeyId and Arn. Put a key policy allowing the account root plus the ecr.amazonaws.com service principal kms:Decrypt/GenerateDataKey/GenerateDataKeyWithoutPlaintext/DescribeKey/CreateGrant/RetireGrant.
2. kms.list_grants(KeyId=<key id>) — confirm no ECR grant exists (baseline).
3. ecr.create_repository(repositoryName='<name>', encryptionConfiguration={'encryptionType':'KMS','kmsKey':<key Arn>}) — observe HTTP 200 with the encryptionConfiguration echoed.
4. Poll kms.list_grants(KeyId=<key id>) (paginating Marker/NextMarker) for up to 300 s and filter for grants naming ECR: on the emulator the list stays empty; on AWS at least one grant with Operations including Decrypt and GenerateDataKey appears within seconds. This poll is where the check burns its budget and is killed at the 60 s harness cap.
5. Then (unreachable today) cloudtrail.lookup_events(LookupAttributes=[{'AttributeKey':'EventName','AttributeValue':'CreateRepository'}]) must yield the caller-attributed ecr.amazonaws.com record with no userIdentity.invokedBy, and the CreateGrant/GenerateDataKey/DescribeKey lookups must yield a kms.amazonaws.com record with the caller's userIdentity.type/arn and userIdentity.invokedBy == 'ecr.amazonaws.com'.
6. ecr.delete_repository(repositoryName='<name>', force=True) and poll kms.list_grants again: the GrantId observed in step 4 must be gone.
7. Control: a repository created with encryptionConfiguration={'encryptionType':'AES256'} must add no grant to the CMK.
