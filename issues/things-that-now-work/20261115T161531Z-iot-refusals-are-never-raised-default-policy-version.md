Issue-Source: trace-sweep-divergence
Issue-Trace: iot-policy-versioning-limits
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-policy-versioning-limits iot-package-sbom-association iot-package-version-sbom-association iot-provisioning-template-error-paths iot-vpce-traj-policy-document-refusals
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DeletePolicyVersion|4xx->2xx|$
Issue-Key: iot|DeletePolicyVersion|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|DeletePolicyVersion|4xx->2xx|status
Issue-Key: iot|DeletePolicyVersion|2xx->4xx|$
Issue-Key: iot|DeletePolicyVersion|2xx->4xx|status
Issue-Key: iot|AssociateSbomWithPackageVersion|4xx->2xx|$.message
Issue-Key: iot|AssociateSbomWithPackageVersion|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|AssociateSbomWithPackageVersion|4xx->2xx|status
Issue-Key: iot|GetPackageVersion|2xx->2xx|$.sbom
Issue-Key: iot|GetPackageVersion|2xx->2xx|$.sbomValidationStatus
Issue-Key: iot|CreateProvisioningClaim|4xx->2xx|$.message
Issue-Key: iot|CreateProvisioningClaim|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|CreateProvisioningClaim|4xx->2xx|status
Issue-Key: iot|CreatePolicy|4xx->4xx|$.message
Issue-Related: 20261107T022027Z-iot-policy-document-plane-10-causes-document-validation.md
Issue-Related: 20261107T022027Z-iot-refusal-envelopes-8-causes-wrong-x-amzn.md
Issue-Commit: 4167e7cf5381813eabd70d9b700d048404e5c90a

# iot refusals are never raised: default policy-version delete, SBOM association, provisioning claim and policy-document validation answer the wrong status

> _Note: this issue was identified against the code base at commit `4167e7cf5381813eabd70d9b700d048404e5c90a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePolicyVersion

## Description
Six recorded refusal/success boundaries answer with the wrong HTTP status: three preconditions AWS enforces are not enforced at all (so the emulator answers 200 where AWS refuses), one refusal is raised where AWS succeeds, one response then carries members AWS does not send, and one validation order is inverted. 6 causes.

Cause 1 of 6: DeletePolicyVersion deletes the policy's default version instead of refusing it.
AWS: Smithy: DeletePolicyVersion errors include DeleteConflictException (client, httpError 409); the operation is DELETE /policies/{policyName}/version/{policyVersionId} with output Unit. Deleting the version that is currently the policy's default is refused: 409, x-amzn-errortype 'DeleteConflictException:http://iot.amazonaws.com/doc/2015-10-02/', message 'Cannot delete a default policy version (name=<policyName>, versionId=<policyVersionId>)' — fixed text 'Cannot delete a default policy version (name=', ', versionId=' and ')' around the policy name and version id taken from the request path.
Emulator: answers 200 with an empty body and removes the version, so the policy is left with no default version.
Why: a client that relies on the default version surviving a sweep of old versions silently destroys the live policy, and code that catches DeleteConflictException to pick a different version never runs.
Fix: the DeletePolicyVersion handler's precondition check against the policy's stored default version, plus its error serializer.
Traces: iot-policy-versioning-limits#task#11.

Cause 2 of 6: DeletePolicyVersion refuses a non-default version as if it were the default.
AWS: Smithy: DeletePolicyVersion output is Unit (200, empty body). Deleting a version that is not the policy's default succeeds.
Emulator: answers 409 DeleteConflictException with body {'__type': 'DeleteConflictException', 'message': 'Cannot delete a default policy version (name=<policyName>, versionId=<policyVersionId>)'} for a version that is not the default at that point in the trajectory.
Why: legitimate version cleanup is impossible — the emulator's idea of which version is default does not follow the policy's state, so clients get a conflict on versions AWS deletes. Hypothesis: the stored default is the first-created version and SetDefaultPolicyVersion is not reflected (see open issue 20261114T091403Z-iot-policy-version-plane-9-causes-setdefaultpolicyversion-is.md, which describes a related policy-version defect but does not carry this step's replay key).
Fix: the policy state's default-version tracking read by the DeletePolicyVersion handler.
Traces: iot-policy-versioning-limits#task#17.

Cause 3 of 6: AssociateSbomWithPackageVersion accepts an SBOM the service cannot read.
AWS: Smithy: AssociateSbomWithPackageVersion (PUT /packages/{packageName}/versions/{versionName}/sbom) errors include ValidationException (client, httpError 400); sbom is @required. When the referenced S3 bucket/key/version is not readable by the service, AWS refuses with 400, x-amzn-errortype 'ValidationException:http://iot.amazonaws.com/doc/2015-10-02/' and the fixed message 'SBOM not accessible by the service. Please make sure the bucket/key/version exists and S3 permission is granted.' (no variable parts).
Emulator: answers 200 and records the association, performing no accessibility pre-flight on the S3 location.
Why: a client that treats 200 as 'the SBOM was accepted and will be validated' never sees the misconfiguration it must fix, and the negative test for an unreadable SBOM cannot be written against the emulator.
Fix: the AssociateSbomWithPackageVersion handler's S3 accessibility pre-flight and its validation-error path.
Traces: iot-package-sbom-association#task#4, iot-package-version-sbom-association#task#4.

Cause 4 of 6: GetPackageVersion then projects sbom and sbomValidationStatus that AWS does not send.
AWS: Smithy: GetPackageVersionResponse has optional members sbom and sbomValidationStatus. After the association in cause 3 is refused, no SBOM is attached, so AWS's GetPackageVersion response omits both members.
Emulator: answers 200 with sbom {'s3Location': {...}} and sbomValidationStatus 'IN_PROGRESS'.
Why: a client polling sbomValidationStatus waits forever on a validation AWS never started, and treats the package version as having a supply-chain document it does not have.
Fix: the GetPackageVersion projection — it must only emit sbom/sbomValidationStatus when an SBOM was actually associated (follows from cause 3's missing pre-flight).
Traces: iot-package-sbom-association#task#5, iot-package-version-sbom-association#task#5.

Cause 5 of 6: CreateProvisioningClaim mints a claim certificate for a template that does not exist.
AWS: Smithy: CreateProvisioningClaim (POST /provisioning-templates/{templateName}/provisioning-claim) errors include ResourceNotFoundException (client, httpError 404); templateName is an @httpLabel. For a template name that names no provisioning template AWS answers 404 with x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/' and message 'Unable to find template named <templateName>' — fixed text 'Unable to find template named ' plus the templateName from the request path.
Emulator: answers 200 with a certificateId/certificatePem/keyPair claim for the non-existent template.
Why: a device fleet-provisioning bootstrap that should fail fast on a mis-named template instead receives a claim certificate that can never be exchanged, so the failure surfaces much later and in the wrong place. (An open issue 'Implement iot CreateProvisioningClaim: unimplemented scaffold stub', 20261114T082236Z-iot-createprovisioningclaim-stub.md, exists, but the operation now answers 200 and that issue does not carry this step's replay key.)
Fix: the CreateProvisioningClaim handler's template lookup and its not-found path.
Traces: iot-provisioning-template-error-paths#task#7.

Cause 6 of 6: CreatePolicy checks the name conflict before validating the policy document.
AWS: Smithy: CreatePolicy errors include MalformedPolicyException (client, httpError 400) and ResourceAlreadyExistsException (client, httpError 409); policyDocument is @required. AWS validates the document first, so a request carrying an unknown IAM condition operator is refused with 400 and the message 'Policy document is Malformed: Invalid Condition type : <conditionType>' — fixed text 'Policy document is Malformed: Invalid Condition type : ' plus the offending condition operator taken from the submitted document — even when the policy name is already taken.
Emulator: answers 409 with x-amzn-errortype 'ResourceAlreadyExistsException' and message 'Policy cannot be created - name already exists (name=<policyName>)': it never validates condition operators, and its conflict check runs first.
Why: a client that submits a corrected document under the same name is told the name is the problem and never learns the document is malformed; validation-ordering contracts (400 before 409) break.
Fix: the CreatePolicy handler's validation ordering plus policy-document condition-operator validation.
Traces: iot-vpce-traj-policy-document-refusals#task#3, #task#4, #task#5, #task#6.

## Evidence
Cause 1 (iot-policy-versioning-limits#task#11): status expected 409, got 200; body expected {'message': 'Cannot delete a default policy version (name=<policyName>, versionId=<versionId>)'}, got {} (empty body); $headers.x-amzn-errortype expected 'DeleteConflictException:http://iot.amazonaws.com/doc/2015-10-02/', got none.
Cause 2 (iot-policy-versioning-limits#task#17): status expected 200, got 409; body expected {}, got {'__type': 'DeleteConflictException', 'message': 'Cannot delete a default policy version (name=<policyName>, versionId=<versionId>)'}.
Cause 3 (iot-package-sbom-association#task#4, also iot-package-version-sbom-association#task#4): status expected 400, got 200; $.message expected 'SBOM not accessible by the service. Please make sure the bucket/key/version exists and S3 permission is granted.', got None; $headers.x-amzn-errortype expected 'ValidationException:http://iot.amazonaws.com/doc/2015-10-02/', got none.
Cause 4 (iot-package-sbom-association#task#5, also iot-package-version-sbom-association#task#5): $.sbom expected None, got {'s3Location': {'bucket': ..., 'key': 'sbom/spdx.json', 'version': '1'}}; $.sbomValidationStatus expected None, got 'IN_PROGRESS'.
Cause 5 (iot-provisioning-template-error-paths#task#7): status expected 404, got 200; $.message expected 'Unable to find template named <templateName>', got None; $headers.x-amzn-errortype expected 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', got none.
Cause 6 (iot-vpce-traj-policy-document-refusals#task#3, with #task#4/#task#5/#task#6 in the same cluster): status expected 400, got 409; $.message expected 'Policy document is Malformed: Invalid Condition type : StringEqualsNope', got 'Policy cannot be created - name already exists (name=<policyName>)'; $headers.x-amzn-errortype got 'ResourceAlreadyExistsException'.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: create a policy, add versions, set one as default, then DELETE /policies/{policyName}/version/{defaultVersionId} (fixture iot-policy-versioning-limits, step task#11) — expect 409 DeleteConflictException, observe 200.
3. Cause 2: in the same trajectory DELETE a non-default version (iot-policy-versioning-limits#task#17) — expect 200 empty body, observe 409 DeleteConflictException.
4. Cause 3: PUT /packages/{packageName}/versions/{versionName}/sbom pointing at an S3 object the service cannot read (iot-package-sbom-association#task#4) — expect 400 ValidationException, observe 200.
5. Cause 4: GET /packages/{packageName}/versions/{versionName} afterwards (iot-package-sbom-association#task#5) — expect no sbom/sbomValidationStatus members, observe sbom + 'IN_PROGRESS'.
6. Cause 5: POST /provisioning-templates/{missingTemplateName}/provisioning-claim (iot-provisioning-template-error-paths#task#7) — expect 404 ResourceNotFoundException, observe 200 with a claim certificate.
7. Cause 6: POST /policies/{existingPolicyName} with a document containing an unknown condition operator (iot-vpce-traj-policy-document-refusals#task#3) — expect 400 MalformedPolicyException naming the invalid condition type, observe 409 ResourceAlreadyExistsException.
