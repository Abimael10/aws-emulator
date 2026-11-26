Issue-Source: trace-sweep-divergence
Issue-Trace: sts-assume-role-malformed-policy-authorized-role
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sts-assume-role-malformed-policy-authorized-role sts-assume-role-with-saml-malformed-policy sts-assume-role-with-web-identity-malformed-policy
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts|AssumeRole|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: sts|AssumeRoleWithSAML|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: sts|AssumeRoleWithWebIdentity|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: sts|AssumeRoleWithWebIdentity|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Related: 20261113T125549Z-sts-assumerolewithsaml-missing-provider-refusal-drops-the-relayed.md
Issue-Commit: 3c9e1d40e4e7a3b63b75dd42df212f92ec4807bb

# sts: malformed inline session-policy JSON is not refused with MalformedPolicyDocument and AWS's parser diagnostic (3 causes)

> _Note: this issue was identified against the code base at commit `3c9e1d40e4e7a3b63b75dd42df212f92ec4807bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssumeRole

## Description
All three STS role-assumption operations that accept an inline session policy (AssumeRole, AssumeRoleWithSAML, AssumeRoleWithWebIdentity) mishandle a Policy parameter whose JSON is syntactically invalid. STS parses the inline policy document BEFORE it evaluates the identity material (SAML assertion / web-identity token / provider existence) and refuses the request with MalformedPolicyDocument, whose Error.Message is the relayed JSON-parser diagnostic that names the specific syntax defect and its position in the submitted document. The emulator either substitutes its own one-line prose for that diagnostic, or never parses the policy at all and instead refuses the request as an identity-token problem. 3 causes, all on service sts.

Shared fault: there is no inline-session-policy JSON parse step that runs ahead of identity validation and that reports the parser's own diagnostic as the MalformedPolicyDocument message.

--- Cause 1 of 3: AssumeRole with a malformed inline Policy returns MalformedPolicyDocument but replaces AWS's parser diagnostic with the fixed sentence "Syntax errors in policy." ---

AWS: Smithy (com.amazonaws.sts#AWSSecurityTokenServiceV20110615, AssumeRole) models Policy as unrestrictedSessionPolicyDocumentType (@length min 1, pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$) and models the refusal MalformedPolicyDocumentException (errorKind client, httpError 400, awsQueryError code 'MalformedPolicyDocument'), documented as "The request was rejected because the policy document was malformed. The error message describes the specific error." The model therefore REQUIRES the message to describe the specific error; the wording itself is recorded behavior only. The recording shows STS relaying its JSON parser's diagnostic verbatim as Error.Message. Its fixed text is: the prefix "Unexpected IOException: ", then the parser's reason clause, then " (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <line>, column: <column>])", then a newline, then " at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <line>, column: <column>]". The literal "REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled)", the bracket/semicolon punctuation and the field names 'Source', 'line', 'column' are FIXED text. The VARIABLE parts come from the submitted Policy string: the reason clause names the construct left unterminated and what was expected (in the recording, "Unexpected end-of-input: expected close marker for Array"), and the two line/column pairs are the offsets of the unterminated construct's start marker and of the failure point in that same document. Status is 400 and Error.Code is 'MalformedPolicyDocument'.

Emulator: status 400 and Error.Code agree, but $.ErrorResponse.Error.Message is exactly "Syntax errors in policy." — a fixed sentence that names neither the defect nor its position, so the parser diagnostic the model promises is absent entirely rather than merely differently worded.

Why: a client that submits a programmatically assembled session policy relies on this message to locate the defect — the construct that was not closed and its line/column in the document it sent. Against the emulator every malformed policy yields the same opaque sentence, so the caller cannot distinguish an unterminated array from, say, a bad escape or a truncated string, and any test or support tool that asserts or parses the AWS diagnostic fails.

Fix: the sts crate's AssumeRole handler and its inline-session-policy validation — parse Policy with a real JSON parser and propagate the parser's diagnostic (reason clause plus both source/line/column markers) into the MalformedPolicyDocumentException message, instead of emitting a canned sentence from the serializer.

Traces: sts-assume-role-malformed-policy-authorized-role#task#2.

--- Cause 2 of 3: AssumeRoleWithSAML with a malformed inline Policy is refused as InvalidIdentityToken (missing SAML provider) instead of MalformedPolicyDocument — the policy is never parsed ---

AWS: Smithy (AssumeRoleWithSAML) models Policy as sessionPolicyDocumentType (@length 1-2048) and models BOTH MalformedPolicyDocumentException (client, httpError 400, awsQueryError code 'MalformedPolicyDocument') and InvalidIdentityTokenException (client, 400, code 'InvalidIdentityToken'). The recording shows the ordering: with a syntactically invalid Policy, STS refuses with Error.Code 'MalformedPolicyDocument' and the same relayed JSON-parser diagnostic described in Cause 1 (fixed prefix "Unexpected IOException: " plus the parser reason clause plus the two "[Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <line>, column: <column>]" markers, whose reason text and line/column values come from the submitted Policy) — the inline policy is parsed BEFORE the PrincipalArn/SAMLAssertion are evaluated, so the identity-side refusal is never reached. Status 400.

Emulator: status 400 matches, but Error.Code is 'InvalidIdentityToken' and Error.Message is the provider-lookup sentence "Specified provider doesn't exist (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlManifestNotFoundException; Request ID: <per-run id>; Proxy: null)". The malformed policy is not diagnosed at all: the handler validates the SAML provider first and reports that instead, so the request is attributed to the wrong refusal class.

Why: the two refusals demand opposite remedies. 'MalformedPolicyDocument' tells the caller to fix the policy document it just built; 'InvalidIdentityToken' tells it to re-fetch an identity token from the IdP. A federation client branching on Error.Code against the emulator retries token acquisition forever while the real fault — its own session policy — is never surfaced, and a test asserting the modeled MalformedPolicyDocument refusal cannot pass.

Fix: the sts crate's AssumeRoleWithSAML handler — move inline-session-policy parsing ahead of SAML provider/assertion validation and raise MalformedPolicyDocumentException with the parser diagnostic (shared with AssumeRole, per Cause 1). Note: the open issue 20261113T125549Z-sts-assumerolewithsaml-missing-provider-refusal-drops-the-relayed concerns the TEXT of the missing-provider refusal for a well-formed request and does not cover this cause — here the missing-provider refusal must not be reached at all.

Traces: sts-assume-role-with-saml-malformed-policy#task#1.

--- Cause 3 of 3: AssumeRoleWithWebIdentity with a malformed inline Policy is refused as InvalidIdentityToken (unparsable JWT) instead of MalformedPolicyDocument — the policy is never parsed ---

AWS: Smithy (AssumeRoleWithWebIdentity) models Policy as sessionPolicyDocumentType (@length 1-2048) and models both MalformedPolicyDocumentException (client, 400, code 'MalformedPolicyDocument') and InvalidIdentityTokenException (client, 400, code 'InvalidIdentityToken'). The recording shows the same ordering as Cause 2: a syntactically invalid Policy is refused with Error.Code 'MalformedPolicyDocument' and the relayed JSON-parser diagnostic (fixed "Unexpected IOException: " prefix plus reason clause plus the two "[Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <line>, column: <column>]" markers, with reason and offsets taken from the submitted Policy), ahead of any validation of WebIdentityToken. Status 400.

Emulator: status 400 matches, but Error.Code is 'InvalidIdentityToken' and Error.Message is the fixed sentence "The ID Token provided is not a valid JWT. (You may see this error if you sent an Access Token)". The policy document is never parsed, so the request is classified as a token defect.

Why: identical to Cause 2 — a web-identity client (mobile/OIDC SDKs and IRSA-style credential providers) sees a token error for a policy error, so it rotates or re-requests the OIDC token instead of fixing the session policy it supplied, and never learns the document is malformed.

Fix: the sts crate's AssumeRoleWithWebIdentity handler — parse the inline session policy before web-identity token validation and raise MalformedPolicyDocumentException carrying the parser diagnostic, sharing the single policy-parse/diagnostic path with AssumeRole and AssumeRoleWithSAML.

Traces: sts-assume-role-with-web-identity-malformed-policy#task#1.

Not covered by any open issue: the open sts issues concern vended credential shape and policy-size members (20261029T191504Z, which owns the other sts clusters in this replay), GetWebIdentityToken (20261112T235120Z / 20261113T034059Z / 20261114T101202Z), GetFederationToken session-policy enforcement (20261113T073604Z, 20261114T014646Z, 20261115T230737Z), EMUL-class access key ids (20261113T093925Z, 20261115T051545Z) and the AssumeRoleWithSAML missing-provider message text (20261113T125549Z). None carries these replay keys and none concerns inline-session-policy JSON parsing or its ordering against identity validation.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up=true, bootstrap_signal=false, score_pct=80.4, steps_total=51, steps_mismatched=10). All three clusters below have stub_path empty (the operations are implemented) and owned_by empty (not covered by any filed issue).

Cause 1 — cluster service=sts|AssumeRole|status=400->400|keys=$.ErrorResponse.Error.Message, fixture sts-assume-role-malformed-policy-authorized-role step task#2, verdict=mismatch, affected_count=1:
  Expected (recorded) HTTP 400, Error.Code 'MalformedPolicyDocument', $.ErrorResponse.Error.Message:
    "Unexpected IOException: Unexpected end-of-input: expected close marker for Array (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 40])\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 41]"
  Actual (emulator) HTTP 400, $.ErrorResponse.Error.Message: "Syntax errors in policy."
  Fixed text in the expected message: "Unexpected IOException: ", "(start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: ", ", column: ", "])", the newline + " at [Source: ...]" tail and the field names Source/line/column. Values from the request: the reason clause ("Unexpected end-of-input: expected close marker for Array" — which construct was unterminated) and the line/column pairs (1/40 and 1/41 in this recording), all computed from the submitted Policy string. The specific numbers are not asserted as literals; the defect is that no diagnostic is produced at all.

Cause 2 — cluster service=sts|AssumeRoleWithSAML|status=400->400|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message, fixture sts-assume-role-with-saml-malformed-policy step task#1, verdict=mismatch, affected_count=1:
  Expected Error.Code 'MalformedPolicyDocument'; actual 'InvalidIdentityToken'.
  Expected Error.Message: the parser diagnostic quoted above; actual: "Specified provider doesn't exist (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlManifestNotFoundException; Request ID: 94195f72-db27-400d-a6a8-504689f80e97; Proxy: null)" (the request id there is a per-run value and is not the point — the refusal class itself is wrong).

Cause 3 — cluster service=sts|AssumeRoleWithWebIdentity|status=400->400|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message, fixture sts-assume-role-with-web-identity-malformed-policy step task#1, verdict=mismatch, affected_count=1:
  Expected Error.Code 'MalformedPolicyDocument'; actual 'InvalidIdentityToken'.
  Expected Error.Message: the parser diagnostic quoted above; actual: "The ID Token provided is not a valid JWT. (You may see this error if you sent an Access Token)".

Model evidence: smithy_query(query="operation", operation="AssumeRole,AssumeRoleWithSAML,AssumeRoleWithWebIdentity") and smithy_query(query="shape", shape="MalformedPolicyDocumentException,InvalidIdentityTokenException") on services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json — Policy member types/length bounds as cited; MalformedPolicyDocumentException client/400/awsQueryError 'MalformedPolicyDocument' with doc "The request was rejected because the policy document was malformed. The error message describes the specific error."; InvalidIdentityTokenException client/400/awsQueryError 'InvalidIdentityToken' with doc about a web identity token that could not be validated. Both are modeled on AssumeRoleWithSAML and AssumeRoleWithWebIdentity; AssumeRole models only the MalformedPolicyDocument one.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) and read the sts divergence clusters.
3. Cause 1: replay fixture sts-assume-role-malformed-policy-authorized-role step task#2 — call AssumeRole against an assumable role with a Policy whose JSON is syntactically invalid (e.g. a Statement array left unterminated). Read the 400 body: the emulator serves Error.Message "Syntax errors in policy."; AWS serves the relayed parser diagnostic "Unexpected IOException: <parser reason> (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <l>, column: <c>])\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <l>, column: <c>]" with the reason and offsets derived from the submitted Policy.
4. Cause 2: replay fixture sts-assume-role-with-saml-malformed-policy step task#1 — call AssumeRoleWithSAML with the same kind of malformed Policy (and a PrincipalArn/SAMLAssertion that satisfy the modeled length bounds). The emulator answers Error.Code 'InvalidIdentityToken' with the "Specified provider doesn't exist (...)" message; AWS answers Error.Code 'MalformedPolicyDocument' with the parser diagnostic, because the policy is parsed before the provider is looked up.
5. Cause 3: replay fixture sts-assume-role-with-web-identity-malformed-policy step task#1 — call AssumeRoleWithWebIdentity with the same malformed Policy. The emulator answers Error.Code 'InvalidIdentityToken' / "The ID Token provided is not a valid JWT. (You may see this error if you sent an Access Token)"; AWS answers Error.Code 'MalformedPolicyDocument' with the parser diagnostic.
6. All three steps must pass — each must serve HTTP 400 with Error.Code 'MalformedPolicyDocument' and a message carrying the parser's reason clause and both Source/line/column markers for the submitted document — before this issue is closed.
