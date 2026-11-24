Issue-Source: trace-sweep-divergence
Issue-Trace: payment-cryptography-resource-policy-workflow-v2
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: payment-cryptography-resource-policy-workflow-v2
Issue-Shape-Id: com.amazonaws.paymentcryptography#PaymentCryptographyControlPlane
Issue-Key: payment-cryptography|GetResourcePolicy|2xx->2xx|$.Policy
Issue-Commit: cbdac01f70b49c509c0eed6a609e985eaaad50c9

# payment-cryptography GetResourcePolicy reports a removed resource policy instead of the empty document

> _Note: this issue was identified against the code base at commit `cbdac01f70b49c509c0eed6a609e985eaaad50c9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourcePolicy

## Description
One cause (Cause 1 of 1).

Cause 1 of 1: a GetResourcePolicy read taken after the key's resource-based policy has been removed still answers the document an earlier PutResourcePolicy attached, instead of the empty-policy document AWS reports for a key that carries no resource-based policy.

AWS: GetResourcePolicy "Returns the resource-based policy attached to an Amazon Web Services Payment Cryptography key" and DeleteResourcePolicy "Removes the resource-based policy attached to an Amazon Web Services Payment Cryptography key" (Smithy docs, com.amazonaws.paymentcryptography#GetResourcePolicy / #DeleteResourcePolicy). Smithy: GetResourcePolicyOutput.Policy is @required, target ResourcePolicy @length 1..20480 — so AWS cannot answer a null/absent Policy for a key that exists, and GetResourcePolicy models no error for an existing key whose policy was removed (its errors are only AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404 for an ARN naming no key, ServiceUnavailableException 503, ThrottlingException 429, ValidationException 400). Recorded behavior for the no-policy state: the recorded response of this read is HTTP 200 with Policy = the empty JSON object `{}` — two characters of FIXED text, carrying nothing from the request or from state; ResourceArn echoes the key ARN from the request. The emulator's own state module already encodes that rule (services/service-com-amazonaws-paymentcryptography-paymentcryptographycontrolplane/src/state/resource_policy.rs: `NO_POLICY_DOCUMENT: &str = "{}"`, documented as what "no policy" is reported as).

Emulator: the step answers HTTP 200 (status matches, so this is a body-only divergence) with $.Policy carrying the still-attached canonical policy document from the earlier PutResourcePolicy in the same workflow — a document granting payment-cryptography:GetPublicKeyCertificate and payment-cryptography:ListTagsForResource to the account root principal — where the recording carries `{}`. Replay detail: `$.Policy: value_mismatch (expected '{}', got '{ "Version": "2012-10-17", "Statement": [ { "Effect": "Allow", "Principal": { "AWS": "arn:aws:iam::<account>:root" }, "Action": [ "payment-cryptography:GetPublicKeyCertificate", "payment-cryptography:ListTagsForResource" ], "Resource": "*" } ] }')`. The account id, the key ARN and the whitespace of that document are per-run values and are not the requirement; the requirement is that the read reports the empty document once no policy is attached.

Hypothesis (cause of the stale read): the removal step in this workflow never reaches the store. DeleteResourcePolicy is still a scaffold stub — services/.../src/ops/delete_resource_policy.rs returns `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteResourcePolicy"))` and persists nothing — so `key.resource_policy` stays `Some(..)` and GetResourcePolicy (src/ops/get_resource_policy.rs, which reports `key.resource_policy_document()`) keeps reporting it. PutResourcePolicy itself does replace correctly (src/ops/put_resource_policy.rs writes `key.resource_policy = Some(attached)`), so the gap is on the removal side, not the write side. The existing open issue 20261113T233803Z-payment-cryptography-deleteresourcepolicy-stub.md covers the 501 of DeleteResourcePolicy itself (behavioral corpus, unit payment-cryptography.resource-policy-put-get-delete-roundtrip); it does NOT carry this wire fixture's replay key, so this GetResourcePolicy divergence is filed separately and must be closed by re-replaying the fixture step named below, not merely by deleting the scaffold marker.

Why: a client that removes a key's resource-based policy and then re-reads it is told the grant is still in force. Policy-reconciliation and least-privilege tooling (drift detection, "is this key still shared?" audits, Terraform/CDK-style refresh) therefore see a cross-principal grant on a key that AWS reports as having no resource policy at all, and conversely cannot observe that a removal took effect — every post-removal assertion against `{}` fails against the emulator while passing against AWS.

Fix: the removal path and the key record's policy field — implement DeleteResourcePolicy's handler so it clears the stored key's resource-policy field (and refuses an ARN naming no key with ResourceNotFoundException 404, per its model), so that the unchanged GetResourcePolicy handler falls through to the state module's empty-policy document for a key with no policy attached. No change is needed in GetResourcePolicy's serializer: it already reports whatever the key record holds.

Traces: payment-cryptography-resource-policy-workflow-v2#task#6 (the failing read; cluster size 1).

## Evidence
Fixture payment-cryptography-resource-policy-workflow-v2, step task#6, operation GetResourcePolicy, verdict mismatch.

Expected (recorded AWS): HTTP 200, body $.Policy == "{}" (the empty JSON object, fixed text), $.ResourceArn == the key ARN from the request.
Actual (emulator): HTTP 200, body $.Policy == the policy document attached earlier in the same workflow:
  {"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account>:root"},"Action":["payment-cryptography:GetPublicKeyCertificate","payment-cryptography:ListTagsForResource"],"Resource":"*"}]}

Replay divergence key: service=payment-cryptography|GetResourcePolicy|status=200->200|keys=$.Policy
No HTTP status or error-code divergence; the diverging member is $.Policy only.

Supporting code: services/service-com-amazonaws-paymentcryptography-paymentcryptographycontrolplane/src/state/resource_policy.rs:31 defines NO_POLICY_DOCUMENT = "{}" as the reported value for a key with no policy; src/ops/get_resource_policy.rs:194 reports key.resource_policy_document(); src/ops/delete_resource_policy.rs:181 still returns Scaffolded::not_implemented("DeleteResourcePolicy") and writes nothing.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire fixture payment-cryptography-resource-policy-workflow-v2.
2. The workflow creates a key, attaches a resource-based policy with PutResourcePolicy, reads it back, then removes it.
3. At step task#6, call GetResourcePolicy for that key ARN after the removal.
4. Observe HTTP 200 with $.Policy carrying the previously attached policy document; the recording has HTTP 200 with $.Policy == "{}".
5. Equivalent by hand: CreateKey -> PutResourcePolicy(ResourceArn=<keyArn>, Policy=<any accepted resource policy>) -> DeleteResourcePolicy(ResourceArn=<keyArn>) -> GetResourcePolicy(ResourceArn=<keyArn>) and expect Policy == "{}".
