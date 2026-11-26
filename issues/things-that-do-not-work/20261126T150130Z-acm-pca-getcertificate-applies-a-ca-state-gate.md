Issue-Source: trace-sweep-divergence
Issue-Trace: acm-pca-ca-not-ready-operation-guards
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: acm-pca-ca-not-ready-operation-guards
Issue-Shape-Id: com.amazonaws.acmpca#ACMPrivateCA
Issue-Key: acm-pca|GetCertificate|4xx->4xx|$.__type
Issue-Key: acm-pca|GetCertificate|4xx->4xx|$.message
Issue-Commit: c7468f96e3a981ee2bb8dab58e6d8ab6beb9a340

# acm-pca GetCertificate applies a CA-state gate AWS does not, answering InvalidStateException instead of ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `c7468f96e3a981ee2bb8dab58e6d8ab6beb9a340`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCertificate

## Description
Cause 1 of 1: GetCertificate refuses on the certificate authority's lifecycle state before it resolves the requested CertificateArn, so a lookup of a certificate that simply does not exist is reported as a CA-state problem instead of a missing resource.

AWS: Smithy (services/service-com-amazonaws-acmpca-acmprivateca, com.amazonaws.acmpca#ACMPrivateCA) models GetCertificate with required input members CertificateAuthorityArn and CertificateArn (both target Arn, @length 5..200, pattern ^arn:[\w+=/,.@-]+:acm-pca:...) and the error set [InvalidArnException, InvalidStateException, RequestFailedException, RequestInProgressException, ResourceNotFoundException], each errorKind=client with httpError 400 (defaulted). ResourceNotFoundException is documented as "A resource such as a private CA, S3 bucket, certificate, audit report, or policy cannot be found."; InvalidStateException as "The state of the private CA does not allow this action to occur." The model does not state which precondition is evaluated first, so for the ordering this is recorded behavior only: in the recording, GetCertificate against a CA that has issued nothing and a CertificateArn that names no issued certificate is answered with HTTP 400, __type "ResourceNotFoundException" and message "Could not find specified certificate." That message is entirely fixed text for this error class — AWS interpolates nothing from the request or from state into it, so a correct emulator emits exactly that string. The recorded transcript therefore shows AWS resolving the certificate identity FIRST and only reporting the CA state when the requested operation actually depends on it; a certificate that was never issued is "not found", not "CA in the wrong state".

Emulator: for the same request the emulator answers HTTP 400 (status matches) but with __type "InvalidStateException" and message "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates." In that emulator message the fixed text is "The certificate authority " ... " is not in the correct state to have issued certificates." and the variable part is the CA ARN echoed from the request's CertificateAuthorityArn (the recording's concrete ARN value, e.g. the one ending /9230d6d6-a9c8-4284-b7c7-1450a364fcc5, is just that run's value and is not part of any requirement). So the emulator has inverted the precedence: it gates on CA readiness unconditionally and never reaches the certificate lookup.

Why: ResourceNotFoundException and InvalidStateException carry opposite retry semantics for an ACM-PCA client. InvalidStateException (together with RequestInProgressException) is the transient, keep-polling branch of a CA bring-up; ResourceNotFoundException is terminal. A client that polls GetCertificate for a certificate it expects to appear, or that treats InvalidStateException as "wait and retry" and ResourceNotFoundException as "give up / create it", loops forever against the emulator instead of concluding the certificate is absent. Error handlers that match on AWS's fixed message text "Could not find specified certificate." never fire, because the emulator's message shares no substring with it.

Note on an existing issue: the open issue 20261125T071918Z-acm-pca-getcertificate-on-a-not-yet-ready.md ("acm-pca GetCertificate on a not-yet-ready CA returns ResourceNotFoundException instead of InvalidStateException") is from the behavioral corpus and asks for the OPPOSITE mapping; it does not carry this fixture's replay key and does not cover this step. The two must be reconciled against the recording: the wire corpus shows that a CA-readiness refusal must not displace the not-found result for a CertificateArn that names no issued certificate, so the state gate has to be narrowed rather than added, and ResourceNotFoundException must remain reachable while the CA is pre-ACTIVE.

Fix: the acm-pca GetCertificate handler's precondition/validation step — order the checks so the CertificateArn is resolved against the certificate store before any CA-status refusal is raised, and emit ResourceNotFoundException with AWS's fixed message "Could not find specified certificate." when it resolves to nothing; reserve InvalidStateException (with its CA-ARN-interpolated message) for the cases the recording actually shows it for. No change to the HTTP status is needed — both errors are 400.

Traces: acm-pca-ca-not-ready-operation-guards#task#8 (GetCertificate, verdict mismatch).

## Evidence
Divergence cluster service=acm-pca|GetCertificate|status=400->400|keys=$.__type,$.message, verdict mismatch, affected_count 1 (acm-pca-ca-not-ready-operation-guards#task#8). stub_path empty, owned_by empty — not blocked by an unimplemented operation and not owned by any open issue's replay key.

Expected (recorded AWS), HTTP 400:
  $.__type   = "ResourceNotFoundException"
  $.message  = "Could not find specified certificate."   (fixed text in full; no request- or state-derived values)

Actual (emulator), HTTP 400:
  $.__type   = "InvalidStateException"
  $.message  = "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates."
               (fixed text: "The certificate authority " + " is not in the correct state to have issued certificates."; variable part: the CA ARN echoed from the request)

Replay detail verbatim:
  $.__type: value_mismatch (expected 'ResourceNotFoundException', got 'InvalidStateException')
  $.message: value_mismatch (expected 'Could not find specified certificate.', got 'The certificate authority arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/9230d6d6-a9c8-4284-b7c7-1450a364fcc5 is not in the correct state to have issued certificates.')

HTTP status is NOT part of the divergence (400 expected, 400 actual); only the error code and message diverge.

Model evidence: GetCertificate errors include both ResourceNotFoundException and InvalidStateException (client, 400). ResourceNotFoundException doc: "A resource such as a private CA, S3 bucket, certificate, audit report, or policy cannot be found." InvalidStateException doc: "The state of the private CA does not allow this action to occur."

Scope of the run: 98 steps replayed across 11 fixtures, 97 passed, this is the single mismatch (score 99.0%), so the surrounding acm-pca CA-lifecycle steps in the same fixture pass — the fault is isolated to this operation's error precedence, not to credentials, routing or the CA state machine.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture acm-pca-ca-not-ready-operation-guards.
2. The fixture creates a private CA and, before that CA has issued any certificate, calls GetCertificate(CertificateAuthorityArn=<that CA's ARN>, CertificateArn=<an ARN under that CA naming no issued certificate>) at step task#8.
3. Observe the response: the emulator returns HTTP 400 with __type "InvalidStateException" and message "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates."
4. Expected per the recording: HTTP 400 with __type "ResourceNotFoundException" and message exactly "Could not find specified certificate."
5. Controls that must keep passing after the fix: the other 97 steps of this corpus, in particular the remaining acm-pca CA-lifecycle steps of the same fixture, and any step where an InvalidStateException refusal is the recorded answer — narrowing the state gate must not remove it where the recording shows it.
6. Re-replay the wire corpus and confirm acm-pca-ca-not-ready-operation-guards#task#8 passes with steps_mismatched 0.
