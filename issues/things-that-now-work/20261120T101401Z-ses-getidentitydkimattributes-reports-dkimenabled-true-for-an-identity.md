Issue-Source: trace-sweep-divergence
Issue-Trace: ses-identity-verification-and-dkim-attributes-by-identity-state
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ses-identity-verification-and-dkim-attributes-by-identity-state
Issue-Shape-Id: com.amazonaws.ses#SimpleEmailService
Issue-Key: ses|GetIdentityDkimAttributes|2xx->2xx|$.GetIdentityDkimAttributesResponse.GetIdentityDkimAttributesResult.DkimAttributes.entry.value.DkimEnabled
Issue-Commit: 6eb53fa801e3042db4570eab6a21985a3debd56e

# SES GetIdentityDkimAttributes reports DkimEnabled true for an identity whose DKIM signing is not enabled

> _Note: this issue was identified against the code base at commit `6eb53fa801e3042db4570eab6a21985a3debd56e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIdentityDkimAttributes

## Description
Cause 1 of 1: GetIdentityDkimAttributes does not track per-identity DKIM-signing state — it reports DkimEnabled=true for an identity that is in a state where DKIM signing is not enabled.

AWS: Smithy model services/service-com-amazonaws-ses-simpleemailservice (SES 2010-12-01, awsQuery protocol). GetIdentityDkimAttributes takes required Identities (IdentityList) and returns required DkimAttributes (map of identity -> IdentityDkimAttributes). Member documentation for IdentityDkimAttributes.DkimEnabled: "Is true if DKIM signing is enabled for email sent from the identity. It's false otherwise. The default value is true." So the value is a readback of that identity's DKIM-signing state: false whenever signing is not enabled for it, and it is only the DEFAULT that is true — not a constant. SetIdentityDkimEnabled is the operation that mutates this state. HTTP 200 for the read is correct per the recording.

Emulator: answers HTTP 200 with a well-shaped GetIdentityDkimAttributesResponse but serializes DkimAttributes.entry.value.DkimEnabled as 'true' for an identity whose recorded state is DkimEnabled=false. Hypothesis: the DkimEnabled field is emitted as a hard-coded/default literal rather than being read out of the identity's stored attributes, so neither an identity that never had DKIM enabled nor one for which enablement was refused is ever reported as false.

Why: DkimEnabled is the flag clients poll to decide whether an identity is ready to send DKIM-signed mail. A client relying on AWS's behavior reads true, concludes Easy DKIM is active for that identity, and skips the enablement/CNAME-publication step — then sends unsigned mail, or in a CI/IaC convergence loop declares a DKIM setup complete that was never performed. It also makes the state unobservable in the other direction: disabling DKIM via SetIdentityDkimEnabled(DkimEnabled=false) cannot be confirmed, because the readback still says true.

Fix: the state/readback path for identity DKIM attributes in the SES service crate — GetIdentityDkimAttributes' handler must project each requested identity's stored DKIM-signing flag into IdentityDkimAttributes.DkimEnabled (defaulting to true only for identities that have no stored value), instead of serializing a constant. The flag must be the same one SetIdentityDkimEnabled writes, so the two operations round-trip. Status 200 and the rest of the response shape are already correct.

Traces: ses-identity-verification-and-dkim-attributes-by-identity-state#task#3 (GetIdentityDkimAttributes, expected 200 / actual 200, diverging key $.GetIdentityDkimAttributesResponse.GetIdentityDkimAttributesResult.DkimAttributes.entry.value.DkimEnabled).

Note: open issue 20261029T171854Z-sesv2-email-identity-dkim-and-mail-from-attributes-are ('Email-identity DKIM and MAIL FROM attributes are wrong or missing on read') describes a similar readback gap, but it is filed against the sesv2 GetEmailIdentity surface and carries no replay key for this step; it does not cover this classic-SES GetIdentityDkimAttributes divergence.

## Evidence
Fixture ses-identity-verification-and-dkim-attributes-by-identity-state, step task#3, operation GetIdentityDkimAttributes, verdict mismatch.

Expected status 200, actual status 200 — the status is correct.

Diverging key $.GetIdentityDkimAttributesResponse.GetIdentityDkimAttributesResult.DkimAttributes.entry.value.DkimEnabled:
  expected: 'false'
  actual:   'true'

The fixture walks one identity through distinct verification/DKIM states and reads the attributes back per state; the emulator returns the same DkimEnabled=true in the state that must report false, i.e. the value does not vary with identity state. Smithy: IdentityDkimAttributes.DkimEnabled is documented as 'true if DKIM signing is enabled ... false otherwise', with true only as the default.

affected_count 1.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay wire fixture 'ses-identity-verification-and-dkim-attributes-by-identity-state' from trace set simparity-wire-aws-v1.
3. The fixture establishes an identity in a state where DKIM signing is not enabled, then at step task#3 calls GetIdentityDkimAttributes with that identity in Identities.
4. Observe DkimAttributes[<identity>].DkimEnabled in the 200 response: the emulator returns true where AWS returns false.
