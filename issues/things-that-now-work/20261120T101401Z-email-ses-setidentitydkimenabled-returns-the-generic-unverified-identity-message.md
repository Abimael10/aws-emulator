Issue-Source: trace-sweep-divergence
Issue-Trace: email-dkim-enablement-gated-by-domain-verification
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: email-dkim-enablement-gated-by-domain-verification
Issue-Shape-Id: com.amazonaws.ses#SimpleEmailService
Issue-Key: email|SetIdentityDkimEnabled|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 6eb53fa801e3042db4570eab6a21985a3debd56e

# SES SetIdentityDkimEnabled returns the generic unverified-identity message instead of the DKIM-specific one

> _Note: this issue was identified against the code base at commit `6eb53fa801e3042db4570eab6a21985a3debd56e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetIdentityDkimEnabled

## Description
Cause 1 of 1: SetIdentityDkimEnabled on a domain identity that has not completed verification is rejected with the wrong error message text.

AWS: Smithy model services/service-com-amazonaws-ses-simpleemailservice (SES 2010-12-01, awsQuery protocol); SetIdentityDkimEnabled takes required members Identity (Identity) and DkimEnabled (Enabled, boolean) and models no operation-specific errors, so the failure is a service-common awsQuery error document. Operation documentation: "You can enable DKIM signing for an identity at any time after you start the verification process for the identity" and, for an email address, "you can only enable DKIM signing if the corresponding domain ... has been set up to use Easy DKIM" — i.e. Easy-DKIM enablement is gated on the domain's DKIM setup, not merely on generic identity existence. Recorded behavior for that gate: HTTP 400 with an ErrorResponse whose Error.Message is "Domain <domain> is not verified for DKIM signing." The fixed text of that message is "Domain " ... " is not verified for DKIM signing."; the only variable part is <domain>, the domain taken verbatim from the request's Identity member. The recorded Error.Code matched the emulator and is not part of this divergence.

Emulator: answers the same HTTP 400 but serializes ErrorResponse.Error.Message as "Identity <identity> is not verified" — a different message class entirely: it says "Identity" rather than "Domain", says "is not verified" rather than "is not verified for DKIM signing", and drops the trailing period. The emulator is evidently reusing one shared "identity not verified" string for every identity-state rejection instead of emitting the DKIM-enablement-specific message for this gate.

Why: a client that distinguishes failure causes by error message — SES's awsQuery errors for this operation are not discriminated by a modeled error shape, so the message is the only signal — cannot tell "this domain has not been set up for Easy DKIM" from "this identity does not exist / is unverified". Retry and remediation logic diverges: the real message directs the caller to complete DKIM setup (VerifyDomainDkim + publish the CNAME records), while the emulator's message directs the caller to re-run plain identity verification, which never clears the condition. Assertions and log scrapers keyed on the documented wording fail against the emulator.

Fix: the error-construction path of the SetIdentityDkimEnabled handler in the SES service crate — the DKIM-enablement precondition must raise its own error with the DKIM-specific message template, rather than falling through to the shared unverified-identity error used by the other identity operations. The message template must interpolate the request's Identity value and keep AWS's fixed text and terminal period; the HTTP status (400) and the error code are already correct and must not change.

Traces: email-dkim-enablement-gated-by-domain-verification#task#2 (SetIdentityDkimEnabled, expected 400 / actual 400, diverging key $.ErrorResponse.Error.Message).

## Evidence
Fixture email-dkim-enablement-gated-by-domain-verification, step task#2, operation SetIdentityDkimEnabled, verdict mismatch.

Expected status 400, actual status 400 — the status is correct.

Diverging key $.ErrorResponse.Error.Message:
  expected: 'Domain sim2real-probe-ses-dkim-unverified-1961259c.example.com is not verified for DKIM signing.'
  actual:   'Identity sim2real-probe-ses-dkim-unverified-1961259c.example.com is not verified'

The generated domain label in both strings is a per-run value and is NOT the requirement. The requirement is the template: fixed text "Domain " + <Identity from the request> + " is not verified for DKIM signing." The emulator substitutes a different fixed text ("Identity " ... " is not verified", no period), which is the defect.

No other key in the response diverged (error code and envelope matched); affected_count 1.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay wire fixture 'email-dkim-enablement-gated-by-domain-verification' from trace set simparity-wire-aws-v1.
3. The fixture first starts verification for a domain identity but does not complete DKIM setup for it, then at step task#2 calls SetIdentityDkimEnabled with Identity=<that domain> and DkimEnabled=true.
4. Observe the 400 ErrorResponse body: Error.Message is 'Identity <domain> is not verified' where AWS returns 'Domain <domain> is not verified for DKIM signing.'
