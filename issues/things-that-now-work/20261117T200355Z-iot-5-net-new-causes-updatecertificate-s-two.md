Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.update-certificate-internal-statuses-refused-register-inactive-round-trips iot.update-certificate-revoked-to-active-rejected iot.thing-versioning-and-recreate-identity iot.deleted-policy-and-policy-version-refuse-repeat-delete-and-read iot.list-things-attribute-value-without-attribute-name-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|UpdateCertificate|InvalidRequestException
Issue-Key: iot.update-certificate-revoked-to-active-rejected|_error#11|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: iot|UpdateThing|VersionConflictException
Issue-Key: iot.deleted-policy-and-policy-version-refuse-repeat-delete-and-read|expect_error#23|AssertionError|assert got == (code, message), f"expected {(code, message)!r}, got {got!r}"
Issue-Key: iot|ListThings|InvalidRequestException
Issue-Commit: eb25315957aceaabd7d1a9e502f713207998e734

# iot: 5 net-new causes — UpdateCertificate's two state guards (internal statuses mis-classified, REVOKED->ACTIVE accepted) plus emulator-authored refusal wording on UpdateThing, DeletePolicy and ListThings

> _Note: this issue was identified against the code base at commit `eb25315957aceaabd7d1a9e502f713207998e734`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateCertificate

## Description
This issue bundles 5 net-new root causes in the iot control plane. Each must be closed before submitting.

Cause 1 of 5 — UpdateCertificate refuses an internally-used newStatus with the wrong error class and wording.
AWS: Smithy: UpdateCertificate (PUT /certificates/{certificateId}?newStatus=) models CertificateStateException (errorKind client, httpError 406, doc 'The certificate operation is not allowed'); CertificateStatus is an enum of ACTIVE, INACTIVE, REVOKED, PENDING_TRANSFER, REGISTER_INACTIVE, PENDING_ACTIVATION, and AWS documents that setting the status to PENDING_TRANSFER or PENDING_ACTIVATION results in an exception. Recorded response: HTTP 406, code CertificateStateException, message 'Not allowed to update to PENDING_ACTIVATION status using UpdateCertificate call'. Fixed text: 'Not allowed to update to ' ... ' status using UpdateCertificate call'; the only variable part is the requested newStatus taken from the request (PENDING_ACTIVATION or PENDING_TRANSFER). The certificate is left in its current status.
Emulator: answers InvalidRequestException (HTTP 400) with the emulator-authored message 'Status PENDING_ACTIVATION is used internally by IoT and is not intended for developer use'. The handler documents this choice itself and states that CertificateStateException is treated as unreachable 'because every status this operation admits may be set from every other one'.
Why: a client that branches on CertificateStateException/406 (the modeled class for a refusal that depends on the certificate's current state) instead sees a 400 InvalidRequestException and classifies an expected state refusal as a malformed request; message-matching and retry logic both take the wrong branch.
Fix: the UpdateCertificate handler's status-transition validation and the state rule it delegates to (update_certificate_status) must classify internal-status targets as CertificateStateException with AWS's wording.
Traces: iot.update-certificate-internal-statuses-refused-register-inactive-round-trips.

Cause 2 of 5 — UpdateCertificate accepts moving a REVOKED certificate back to ACTIVE.
AWS: Smithy: UpdateCertificate models CertificateStateException (406) exactly for a refusal that depends on the certificate's current status. Recorded response: HTTP 406, code CertificateStateException, message 'Not allowed to update from REVOKED status using UpdateCertificate call' — fixed text 'Not allowed to update from ' ... ' status using UpdateCertificate call', the variable part being the certificate's current status taken from the emulator's state (REVOKED); the certificate stays REVOKED afterwards. The transitions leading up to it (ACTIVE->INACTIVE->REVOKED) succeed.
Emulator: returns HTTP 200 with an empty body and silently moves the certificate from REVOKED back to ACTIVE.
Why: revocation is not terminal in the emulator, so a revoked device credential can be re-enabled by a plain UpdateCertificate call. A client relying on AWS's behavior believes a revoked certificate can never be reactivated and gets a fleet whose security state differs from the one AWS would produce — and gets no refusal to surface.
Fix: same component as cause 1 — the UpdateCertificate handler's transition guard/state rule must refuse any transition out of REVOKED.
Traces: iot.update-certificate-revoked-to-active-rejected.
(The open issue 20261108T051259Z-iot-certificate-plane-4-causes-deleted-certificates-stay.md claims the opposite symptom — that status updates never apply — and does not cover either cause above: here the updates do apply, and the two guards are missing/mis-classified.)

Cause 3 of 5 — UpdateThing's stale-expectedVersion refusal message is emulator-authored and leaks version numbers.
AWS: Smithy: UpdateThing (PATCH /things/{thingName}) models VersionConflictException (errorKind client, httpError 409, doc 'An exception thrown when the version of an entity specified with the expectedVersion parameter does not match the latest version in the system'). Recorded response: 409, code VersionConflictException, message 'The version for thing <thingName> does not match the expected version.' — fixed text 'The version for thing ' ... ' does not match the expected version.' with the thingName taken from the request; no version numbers appear in the message.
Emulator: returns the right code but the message 'expectedVersion 1 does not match the version 2 of thing <thingName>', which is emulator-authored and interpolates both the request's expectedVersion and the stored version.
Why: clients and operators that match or log the AWS message see different text, and the emulator exposes internal version values AWS does not publish in this refusal.
Fix: the UpdateThing handler's version-guard refusal message.
Traces: iot.thing-versioning-and-recreate-identity.
(Related but distinct from the open 20261115T093926Z-iot-updatething-s-resourcenotfoundexception-message-is-account-qualified.md, which covers UpdateThing's ResourceNotFoundException wording, not its VersionConflictException.)

Cause 4 of 5 — DeletePolicy's not-found refusal message is emulator-authored.
AWS: Smithy: DeletePolicy (DELETE /policies/{policyName}) models ResourceNotFoundException (errorKind client, httpError 404). Recorded response for a policy that does not exist (here: one just deleted): 404, code ResourceNotFoundException, message exactly 'Policy not found' — the whole message is fixed text and carries no policy name. (AWS words the sibling read differently, 'Policy default version not found' for GetPolicy, which the emulator already matches.)
Emulator: message 'Policy <policyName> does not exist', interpolating the requested name.
Why: a client that distinguishes AWS's two policy not-found sentences, or asserts on the delete one, reads a message AWS never sends; the repeat-delete path cannot be recognized.
Fix: the DeletePolicy handler's not-found refusal message.
Traces: iot.deleted-policy-and-policy-version-refuse-repeat-delete-and-read.

Cause 5 of 5 — ListThings' attributeValue-without-attributeName refusal message is emulator-authored.
AWS: Smithy: ListThings (GET /things) takes attributeName and attributeValue as httpQuery members and models InvalidRequestException (errorKind client, httpError 400). Recorded response when attributeValue is supplied without attributeName: 400, code InvalidRequestException, message exactly 'An attributeName is required to search for Things by attributeValue.' — the whole message is fixed text, with no request values in it.
Emulator: returns the right code with the message 'attributeValue can only be used with attributeName'.
Why: the wording differs from AWS for the same error class, so clients that surface or match the message diverge; the operation itself is otherwise correct (the two-parameter filter works).
Fix: the ListThings handler's input validation refusal message.
Traces: iot.list-things-attribute-value-without-attribute-name-rejected.

## Evidence
Cause 1: expected error code 'CertificateStateException' (HTTP 406) with message 'Not allowed to update to PENDING_ACTIVATION status using UpdateCertificate call' (and the PENDING_TRANSFER variant); got 'InvalidRequestException' (HTTP 400) with 'Status PENDING_ACTIVATION is used internally by IoT and is not intended for developer use'. Handler self-documents the choice: services/service-com-amazonaws-iot-awsiotservice/src/ops/update_certificate.rs lines 14-26.

Cause 2: expected a ClientError (CertificateStateException, 406, 'Not allowed to update from REVOKED status using UpdateCertificate call') on UpdateCertificate(certificateId=<id>, newStatus='ACTIVE') while the certificate is REVOKED; got success: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '0', ...}}}. Component: services/service-com-amazonaws-iot-awsiotservice/src/ops/update_certificate.rs and the state rule update_certificate_status in services/service-com-amazonaws-iot-awsiotservice/src/state.rs.

Cause 3: expected message 'The version for thing <thingName> does not match the expected version.'; got 'expectedVersion 1 does not match the version 2 of thing <thingName>' (code VersionConflictException matched). Component: services/service-com-amazonaws-iot-awsiotservice/src/ops/update_thing.rs.

Cause 4: expected ('ResourceNotFoundException', 'Policy not found'); got ('ResourceNotFoundException', 'Policy <policyName> does not exist'). Component: services/service-com-amazonaws-iot-awsiotservice/src/ops/delete_policy.rs.

Cause 5: expected ('InvalidRequestException', 'An attributeName is required to search for Things by attributeValue.'); got ('InvalidRequestException', 'attributeValue can only be used with attributeName'). Component: services/service-com-amazonaws-iot-awsiotservice/src/ops/list_things.rs.

## Steps to Reproduce
Cause 1: iot create-keys-and-certificate --set-as-active; then update-certificate --certificate-id <id> --new-status PENDING_ACTIVATION (and again with PENDING_TRANSFER). Observe 400 InvalidRequestException instead of 406 CertificateStateException 'Not allowed to update to PENDING_ACTIVATION status using UpdateCertificate call'; the certificate must stay ACTIVE.

Cause 2: with the same certificate, update-certificate --new-status INACTIVE, then --new-status REVOKED (both succeed and are reflected by describe-certificate), then update-certificate --new-status ACTIVE. Observe HTTP 200 and the status changing, instead of 406 CertificateStateException 'Not allowed to update from REVOKED status using UpdateCertificate call' with the status left REVOKED.

Cause 3: iot create-thing --thing-name T --attribute-payload attributes={env=v1}; update-thing --thing-name T --expected-version 1 --attribute-payload attributes={env=v2} (version becomes 2); update-thing --thing-name T --expected-version 1 ... again. Observe the VersionConflictException message wording.

Cause 4: iot create-policy --policy-name P --policy-document '<any valid doc>'; delete-policy --policy-name P; delete-policy --policy-name P again. Observe the ResourceNotFoundException message wording.

Cause 5: iot create-thing --thing-name T --attribute-payload attributes={sim2real-marker=M}; list-things --attribute-value M (no --attribute-name). Observe the InvalidRequestException message wording.

Then replay the checks in trace_ids.
