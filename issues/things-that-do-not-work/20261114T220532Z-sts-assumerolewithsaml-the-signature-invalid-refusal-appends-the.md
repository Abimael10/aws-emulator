Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sts.assume-role-with-saml-invalid-identity-token-rejected sts.assume-role-with-saml-rejects-xpath-based-transforms
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts.assume-role-with-saml-invalid-identity-token-rejected|run#11|AssertionError|assert refusal == ("InvalidIdentityToken", _BAD_SIGNATURE_MESSAGE), f"got {refusal!r}"
Issue-Key: sts.assume-role-with-saml-rejects-xpath-based-transforms|run#9|AssertionError|assert message == "Response signature invalid", f"[{label}] the refusal message is {message!r}"
Issue-Commit: d33d509d5311768bd1282d02eb2794d638dad9b8

# sts AssumeRoleWithSAML: the signature-invalid refusal appends the relayed AWSOpenIdDiscoveryService envelope that AWS does not serve for that class, so Error.Message never equals "Response signature invalid"

> _Note: this issue was identified against the code base at commit `d33d509d5311768bd1282d02eb2794d638dad9b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssumeRoleWithSAML

## Description
1 net-new root cause on service sts, grounded in the two failing checks under the `### sts` heading that are neither ALREADY FILED nor OWNED. Both exercise the same refusal class — an assertion whose XML-DSig signature does not validate against the trusted IdP certificate — and both diverge in exactly the same field, Error.Message.

Cause 1 of 1: AssumeRoleWithSAML's bad-signature refusal carries a trailing relayed-exception envelope that AWS omits for this refusal class.
  AWS: Smithy (services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json, smithy_query(operation="AssumeRoleWithSAML")) models RoleArn, PrincipalArn (arnType, @length 20-2048) and SAMLAssertion (SAMLAssertionType, @length 4-100000, @sensitive) as required, and lists InvalidIdentityTokenException with httpError 400 and awsQueryError code `InvalidIdentityToken`. The model states no message text, so the wording is recorded behavior only: for an assertion that is well-formed, inside its validity window, with matching Role/RoleSessionName claims, but whose SignatureValue no longer validates against the registered IdP certificate — and likewise for a signature whose declared Transforms include an XPath-family algorithm STS does not accept — the recorded response is HTTP 400, Error.Code `InvalidIdentityToken`, and Error.Message exactly the sentence `Response signature invalid`. That message is ENTIRELY fixed text for this class: it carries no request-derived value, no id, no timestamp, and — unlike other InvalidIdentityToken classes — no trailing parenthesized envelope. The refusal is deterministic: an identical retry of the same assertion yields the same code and the same message.
  Emulator: HTTP 400 and Error.Code `InvalidIdentityToken` agree, and the refusal decision itself is right (the correctly-signed positive control succeeds in both checks, and the XPath-family variants, whose Reference digests the check proves byte-identical to the accepted baseline, are refused rather than accepted). What diverges is only Error.Message: the emulator serves the fixed sentence followed by an extra relayed envelope — `Response signature invalid (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlInvalidSamlResponseException; Request ID: <per-run uuid>; Proxy: null)`. The whole parenthesized suffix — the five `Key: Value` fields and their punctuation — is additional text AWS does not serve here; the per-run Request ID inside it is not the divergence, its presence is.
  Why: a client that matches the documented AWS sentence to tell "the assertion's signature does not verify" from the other InvalidIdentityToken classes (an un-decodable base64 payload, a missing mandatory signature, a deleted SAML manifest) never matches it, because the message no longer equals any fixed string — it ends in a uuid that changes per call. Federation login code, SDK-level error assertions, log scrapers and support tooling that compare or group on Error.Message see an unstable, never-equal message for a refusal AWS words identically every time.
  Fix: the sts AssumeRoleWithSAML handler's signature-validation branch and the error serializer that composes Error.Message for InvalidIdentityTokenException: the relayed-envelope suffix must be attached per refusal class, not to every InvalidIdentityToken. Resolved issue 20261113T141854Z-sts-assumerolewithsaml-refusal-messages-4-net-new-causes.md required that envelope for the non-base64, unsigned-assertion and deleted-provider classes; it is now applied to the signature-invalid class as well, where the recording shows the bare sentence. The nearest OPEN issue, 20261113T125549Z-sts-assumerolewithsaml-missing-provider-refusal-drops-the-relayed.md, covers the missing-provider class (and the opposite direction, a dropped envelope) and carries neither of the check ids below, so a fix to it closes nothing here.
  Traces: sts.assume-role-with-saml-invalid-identity-token-rejected (tampered SignatureValue, asserted twice for determinism) and sts.assume-role-with-saml-rejects-xpath-based-transforms (label `xpath-1.0-true`, Transform http://www.w3.org/TR/1999/REC-xpath-19991116 with the no-op expression `true()`).

## Evidence
Replay of simparity-behavioral-aws-v1 against the booted emulator: 154/239 passed, bootstrap_signal false (the emulator is serving). Per-service line `sts 112/145 (77%) 33 failed`; 31 of those 33 are marked OWNED or ALREADY FILED, and the two below are neither.

[mismatch] sts.assume-role-with-saml-invalid-identity-token-rejected
  check_sts_assume_role_with_saml_invalid_identity_token_rejected.py line 75:
  assert refusal == ("InvalidIdentityToken", _BAD_SIGNATURE_MESSAGE)
  AssertionError: got ('InvalidIdentityToken', 'Response signature invalid (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlInvalidSamlResponseException; Request ID: 50082b44-69f1-4a74-8173-9fe9417ddb60; Proxy: null)')
  Expected: ('InvalidIdentityToken', 'Response signature invalid') — _BAD_SIGNATURE_MESSAGE is the bare sentence, compared for equality without any normalization, so the trailing envelope (not just its uuid) is the whole divergence.
  The positive control in the same run passed: the correctly signed assertion for the same provider/role returned Credentials with an AccessKeyId, so provider registration, trust policy, claim matching and signature verification all work; only the refusal message diverges.

[mismatch] sts.assume-role-with-saml-rejects-xpath-based-transforms
  check_sts_assume_role_with_saml_rejects_xpath_based_transforms.py line 121:
  assert message == "Response signature invalid"
  AssertionError: [xpath-1.0-true] the refusal message is 'Response signature invalid (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlInvalidSamlResponseException; Request ID: d86bf18b-d67b-4c09-96fb-80d732f221a4; Proxy: null)'
  The earlier assertions in that check passed: the plain signed assertion was accepted (line 118 equivalent positive control), the variant's Reference DigestValue was proven byte-identical to the baseline's, the call WAS refused (code is not None) and the code WAS InvalidIdentityToken. Only the message comparison failed.

In both checks the HTTP status (400) and Error.Code (InvalidIdentityToken) match AWS; the divergence is confined to the trailing envelope added to Error.Message. The two uuids quoted above are per-run values and are not part of the requirement — the requirement is that this class's message is exactly the fixed sentence `Response signature invalid`.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 and re-run the two check ids in trace_ids (both listed under the report heading `sts`).
3. Or directly, against the emulator:
   a. iam CreateSAMLProvider with an IdP metadata document carrying a self-signed RSA certificate, and iam CreateRole with a trust policy whose Principal is that provider ARN and whose Action is sts:AssumeRoleWithSAML.
   b. Build a SAMLResponse (Exclusive-C14N, enveloped signature, RSA-PKCS1v15/SHA-256) for that role/provider with a valid NotBefore/NotOnOrAfter window and matching Role attribute; call sts AssumeRoleWithSAML and confirm it returns Credentials (positive control).
   c. Re-send the identical response with its SignatureValue tampered so it no longer validates against the registered certificate. Expected: HTTP 400, Error.Code `InvalidIdentityToken`, Error.Message exactly `Response signature invalid` with NO trailing parenthesized text. Actual: the same sentence plus ` (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlInvalidSamlResponseException; Request ID: <uuid>; Proxy: null)`.
   d. Repeat (c) with the identical assertion: the same code and the same message must come back, and that message must be string-equal to the one from (c).
   e. Variant: re-send a digest-identical response whose ds:SignedInfo declares an extra Transform of http://www.w3.org/TR/1999/REC-xpath-19991116 (expression `true()`), and again of http://www.w3.org/2002/06/xmldsig-filter2 (union over `/`). Each must be refused with the same `InvalidIdentityToken` / `Response signature invalid` pair, with no envelope.
4. Both checks must pass — and the refusal classes that DO carry the envelope (non-base64 payload, unsigned assertion, deleted provider; see resolved issue 20261113T141854Z) must keep it — before this issue is closed.
