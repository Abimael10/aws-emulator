Issue-Source: trace-sweep-divergence
Issue-Trace: sts-assume-role-with-saml-malformed-policy
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sts-assume-role-with-saml-malformed-policy
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts|AssumeRoleWithSAML|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Related: 20261117T113154Z-sts-malformed-inline-session-policy-json-is-not.md
Issue-Commit: ac15f0339b93ea79f15fb1aa9f0f8cf2aa14317f

# sts AssumeRoleWithSAML refuses a malformed inline Policy as InvalidIdentityToken instead of MalformedPolicyDocument

> _Note: this issue was identified against the code base at commit `ac15f0339b93ea79f15fb1aa9f0f8cf2aa14317f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssumeRoleWithSAML

## Description
1 cause, on service sts.

Cause 1 of 1: AssumeRoleWithSAML with a syntactically invalid inline session Policy is classified as an identity-token/provider problem, so the policy document is never parsed and the modeled MalformedPolicyDocument refusal is never produced.

AWS: Smithy model services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json, operation AssumeRoleWithSAML. Its input member Policy targets sessionPolicyDocumentType (@length min 1, max 2048, pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$), and the operation models BOTH MalformedPolicyDocumentException (errorKind client, httpError 400, awsQueryError code 'MalformedPolicyDocument', documented "The request was rejected because the policy document was malformed. The error message describes the specific error.") and InvalidIdentityTokenException (client, httpError 400, awsQueryError code 'InvalidIdentityToken', documented "The web identity token that was passed could not be validated by Amazon Web Services. Get a new identity token from the identity provider and then retry the request."). The ORDERING is recorded behavior only: the recording shows that when Policy is not valid JSON, STS parses the inline session policy BEFORE it evaluates PrincipalArn/SAMLAssertion or looks up the SAML provider, and answers HTTP 400 with Error.Code 'MalformedPolicyDocument' and Error.Message set to its JSON parser's relayed diagnostic. In that message the FIXED text is the prefix "Unexpected IOException: ", then the parser reason clause, then " (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <line>, column: <column>])", then a newline and " at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <line>, column: <column>]" — the literal "REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled)", the bracket/semicolon punctuation and the field names Source/line/column are fixed. The VARIABLE parts are derived from the submitted Policy string: the reason clause naming the unterminated construct and what was expected (in this recording "Unexpected end-of-input: expected close marker for Array"), and the two line/column pairs (the start marker of the unterminated construct and the failure point). Those numbers are one run's values filling the template, not the requirement.

Emulator: status 400 matches, but the refusal class is wrong. $.ErrorResponse.Error.Code is 'InvalidIdentityToken' (expected 'MalformedPolicyDocument') and $.ErrorResponse.Error.Message is the provider-lookup sentence "Specified provider doesn't exist (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlManifestNotFoundException; Request ID: <per-run id>; Proxy: null)" instead of the parser diagnostic. The per-run request id inside that message is not the point; the point is that no policy parse happens at all, so the request is attributed to the SAML provider rather than to the malformed document.

Why: the two refusals demand opposite remedies. 'MalformedPolicyDocument' tells a federation client to fix the session policy it just assembled; 'InvalidIdentityToken' tells it to re-fetch an assertion from the IdP. A client branching on Error.Code against the emulator retries identity acquisition indefinitely while its real fault — its own inline policy — is never surfaced, and it never receives the position/reason diagnostic AWS promises for locating the defect. Any test asserting the modeled MalformedPolicyDocument refusal for this operation cannot pass.

Fix: the sts crate's AssumeRoleWithSAML handler and its inline-session-policy validation — parse Policy as JSON before SAML provider/assertion validation runs, and raise MalformedPolicyDocumentException carrying the parser's own reason clause and both Source/line/column markers, sharing one policy-parse/diagnostic path with the other role-assumption operations.

Related open issue, which does NOT own this failure: 20261117T113154Z-sts-malformed-inline-session-policy-json-is-not.md describes the same shared fault and names this fixture in its trace list, but its recorded replay keys cover sts|AssumeRoleWithSAML|4xx->4xx|$.ErrorResponse.Error.Code only, not this cluster's combined Error.Code + Error.Message key, so the replay reports this cluster as unowned (owned_by empty) and it is filed here with its own replay key.

Traces: sts-assume-role-with-saml-malformed-policy#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up=true, bootstrap_signal=false, score_pct=80.4, steps_total=51, steps_passed=41, steps_mismatched=10, divergence_count=9).

Cluster: divergence_key service=sts|AssumeRoleWithSAML|status=400->400|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message; fixture sts-assume-role-with-saml-malformed-policy, step task#1; verdict=mismatch; affected_count=1; stub_path empty (the operation IS implemented); owned_by empty (no filed issue carries this replay key).

Expected (recorded AWS) HTTP 400:
  $.ErrorResponse.Error.Code = 'MalformedPolicyDocument'
  $.ErrorResponse.Error.Message = "Unexpected IOException: Unexpected end-of-input: expected close marker for Array (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 40])\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 41]"

Actual (emulator) HTTP 400:
  $.ErrorResponse.Error.Code = 'InvalidIdentityToken'
  $.ErrorResponse.Error.Message = "Specified provider doesn't exist (Service: AWSOpenIdDiscoveryService; Status Code: 400; Error Code: AuthSamlManifestNotFoundException; Request ID: 2ee34c79-aa35-4cb0-ab34-97e4717194cb; Proxy: null)"

Message decomposition for the expected refusal: fixed text = "Unexpected IOException: ", " (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: ", ", column: ", "])", the newline plus " at [Source: ...]" tail, and the field names Source/line/column. Values taken from the request's Policy string: the parser reason clause ("Unexpected end-of-input: expected close marker for Array") and the two line/column pairs (1/40 and 1/41 in this run). The request id in the emulator's actual message is a per-run value and is not asserted.

Model evidence: smithy_query(query="operation", operation="AssumeRoleWithSAML") and smithy_query(query="shape", shape="MalformedPolicyDocumentException,InvalidIdentityTokenException,sessionPolicyDocumentType") on services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json — Policy -> sessionPolicyDocumentType @length 1..2048; MalformedPolicyDocumentException client/400/awsQueryError 'MalformedPolicyDocument'; InvalidIdentityTokenException client/400/awsQueryError 'InvalidIdentityToken'; both bound to AssumeRoleWithSAML.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) and read the sts divergence clusters.
3. Replay fixture sts-assume-role-with-saml-malformed-policy step task#1 — call AssumeRoleWithSAML with a RoleArn and PrincipalArn satisfying arnType (@length 20..2048), a SAMLAssertion satisfying SAMLAssertionType (@length 4..100000), and a Policy whose JSON is syntactically invalid (e.g. a Statement array left unterminated) but which satisfies sessionPolicyDocumentType (@length 1..2048).
4. Read the 400 body. The emulator serves Error.Code 'InvalidIdentityToken' with the "Specified provider doesn't exist (...)" message. AWS serves Error.Code 'MalformedPolicyDocument' with Error.Message "Unexpected IOException: <parser reason> (start marker at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <l>, column: <c>])\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: <l>, column: <c>]", with the reason clause and offsets computed from the submitted Policy.
5. Confirm the ordering independently: submit the same malformed Policy together with a PrincipalArn naming a SAML provider that DOES exist. AWS still answers MalformedPolicyDocument, because the policy parse precedes provider lookup.
6. The step must serve HTTP 400 with Error.Code 'MalformedPolicyDocument' and a message carrying the parser's reason clause and both Source/line/column markers for the submitted document before this issue is closed.
