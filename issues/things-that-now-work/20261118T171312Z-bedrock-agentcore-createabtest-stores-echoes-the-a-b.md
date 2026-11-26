Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.create-ab-test-duplicate-name-blocked bedrock-agentcore.create-abtest-invalid-variant-weights-rejected
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.create-ab-test-duplicate-name-blocked|run#11|AssertionError|assert created["name"] == name.lower(), (
Issue-Key: bedrock-agentcore.create-abtest-invalid-variant-weights-rejected|run#11|AssertionError|assert created["name"] == good_name.lower(), (
Issue-Commit: 0a24b18ae59262f0cdae7b98999a169258844aef

# bedrock-agentcore: CreateABTest stores/echoes the A/B test name verbatim instead of lower-casing it, and both A/B-test refusal messages are worded differently from AWS (3 causes)

> _Note: this issue was identified against the code base at commit `0a24b18ae59262f0cdae7b98999a169258844aef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateABTest

## Description
THREE net-new root causes in the bedrock-agentcore A/B test control surface, all reached through CreateABTest. Both failing checks carry SERVICES = ["bedrock-agentcore"], bedrock-agentcore is bootstrapped and answering (40/64 of its checks pass in this run, bootstrap_signal false), and neither check is marked ALREADY FILED or OWNED in the replay report, so these are fidelity defects and not a bootstrap gap.

RELATED, NOT COVERING: 20261111T004500Z-bedrock-agentcore-the-a-b-test-lifecycle-createabtest.md (open) covers a DIFFERENT cause — the A/B-test lifecycle check never reaching CreateABTest because bedrock-agentcore-control CreateOnlineEvaluationConfig is a 501 scaffold stub. The two checks named here DO reach CreateABTest, get a 202, and then diverge on the response payload, so that issue does not cover them.

Cause 1 of 3: CreateABTest does not normalise the A/B test name, so the name it stores and reports back is the caller's spelling rather than the lower-cased one, and the account-uniqueness check is therefore case-sensitive.
AWS: recorded behavior only — the Smithy model (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json) models CreateABTest as POST /ab-tests, success 202, with required name (ABTestName, pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}$ — which permits upper case on INPUT) and an optional output member name; it says nothing about normalisation. The recordings show AWS accepting a mixed-case name and reporting the name member back LOWER-CASED on CreateABTest's own 202 response, and the same lower-cased spelling from GetABTest and in ListABTests summaries. The general rule is: whatever case the request used, the name AWS persists and reports is that name mapped to lower case; because the stored name is the lower-cased one, name uniqueness within the account is effectively case-insensitive (two creates differing only in case collide).
Emulator: the response's name is the request's name byte-for-byte. Both checks fail on that: `CreateABTest normalises the name to lower case; expected 'sim2realdupabtest5021364723', got 'Sim2realDupAbTest5021364723'` and `CreateABTest is expected to lowercase the name: sent 'Sim2realGoodAbTest5021192350', expected 'sim2realgoodabtest5021192350', got 'Sim2realGoodAbTest5021192350'`. The handler passes input.name through unchanged into the stored record, and the uniqueness test compares the verbatim strings, so two names differing only in case are treated as two distinct tests.
Why: a client that creates a test with any upper-case character and then finds it by the name AWS reports (GetABTest/ListABTests lookups, dashboards keyed on name, a second create expecting the documented ConflictException) does not match on the emulator: on AWS the name comes back lower-cased and a case-variant re-create conflicts, on the emulator the name comes back mixed-case and a case-variant re-create silently mints a SECOND test under what AWS considers the same name.
Fix: the CreateABTest handler and the A/B-test state it writes — normalise the name once on the write path so the record, the 202 response, GetABTest and ListABTests all report the normalised spelling, and make the account-uniqueness lookup use the normalised name (see citations under Traces).

Cause 2 of 3: the ConflictException a duplicate A/B-test name is refused with is worded differently from AWS.
AWS: recorded behavior only (the model documents only that ABTestName "must be unique within your account" and that ConflictException is HTTP 409). The recorded refusal is Code ConflictException, HTTP 409, Message whose fixed text is `AB test with name '` + the name + `' already exists` — i.e. the fixed template is "AB test with name '<name>' already exists", and the only variable part is the name, taken from the REQUEST as the caller spelled it (not the lower-cased stored form).
Emulator: the handler formats `An A/B test with the name {name} already exists` — different fixed text ("An A/B test with the name" vs "AB test with name") and no quoting of the name. The status (409) and code (ConflictException) are right; the message is not. Not yet observed in replay only because Cause 1 aborts the check one assertion earlier; the same check asserts this message exactly on its next line.
Why: a client or test that matches on the refusal message to distinguish a name collision from other 409 conflicts does not match, and the message a user is shown differs from AWS's.
Fix: the message helper the create (and update) refusal uses, in the A/B-test state module, so both callers word the condition the same way.

Cause 3 of 3: the ValidationException for variant weights that do not sum to 100 appends the observed total, so its message is not AWS's.
AWS: recorded behavior only (the model documents Variant$weight as the percentage of traffic and that weights across all variants must sum to 100; ValidationException is HTTP 400). The recorded refusal for two in-range weights that do not total 100 is Code ValidationException, HTTP 400, Message exactly the fixed text `Variant weights must sum to 100` — the whole message is fixed text, with no variable part and in particular no echo of the submitted total.
Emulator: the message is `Variant weights must sum to 100, but they sum to <total>`, with the request's total appended — a message AWS never emits. Again not yet observed in replay only because Cause 1 aborts the check earlier; the weights check asserts this exact message.
Why: a client matching the modeled refusal message for a bad traffic split sees an unexpected message, and anything comparing messages for equality (as the recording does) treats a correct refusal as a divergence.
Fix: the weight-total refusal message helper in the A/B-test state module, shared by CreateABTest and UpdateABTest.

Traces: bedrock-agentcore.create-ab-test-duplicate-name-blocked and bedrock-agentcore.create-abtest-invalid-variant-weights-rejected (both [mismatch]). Code citations: services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/create_ab_test.rs:272-282 (verbatim-name uniqueness test and the conflict message), :286-299 (input.name stored unchanged), :174-180 (the name reported back), and services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/state/ab_tests.rs:110-112 (weight-total message), :135 and :190 (the stored name and the id minted from it), :422-424 (ab_test_named lookup).

## Evidence
Suite run: 41/68 passed; bedrock-agentcore 40/64 (62%). bootstrap_signal false.

1) bedrock-agentcore.create-ab-test-duplicate-name-blocked — verdict mismatch:
   AssertionError: CreateABTest normalises the name to lower case; expected 'sim2realdupabtest5021364723', got 'Sim2realDupAbTest5021364723'
   The check sent name='Sim2realDupAbTest<digits>' (mixed case, matching ^[a-zA-Z][a-zA-Z0-9_]{0,47}$) with a gateway, role, 2 variants (C/T1, weights 50/50) and an evaluation config; CreateABTest answered 202 with name echoed verbatim.
   expected: response name == the request name mapped to lower case; actual: the request name unchanged.
   Not reached, and therefore still unverified (re-replay this check after the fix): the second CreateABTest with the same name refused with ConflictException (HTTP 409) and Message == "AB test with name '<name as sent>' already exists"; GetABTest on the original still returning the same abTestId and the lower-cased name. The handler currently formats "An A/B test with the name <name> already exists" (create_ab_test.rs:276-279), which is a different message from the recorded one.

2) bedrock-agentcore.create-abtest-invalid-variant-weights-rejected — verdict mismatch:
   AssertionError: CreateABTest is expected to lowercase the name: sent 'Sim2realGoodAbTest5021192350', expected 'sim2realgoodabtest5021192350', got 'Sim2realGoodAbTest5021192350'
   Same positive control (weights 50/50) as above; the name assertion fails on the 202 response.
   Not reached, and therefore still unverified: CreateABTest with weights 60/60 refused with Code ValidationException and Message exactly 'Variant weights must sum to 100'; and ListABTests then showing the rejected test's lower-cased name ABSENT while the positive control's lower-cased name is PRESENT (with the names not lower-cased, both membership assertions are meaningless). The emulator's message for that condition is 'Variant weights must sum to 100, but they sum to 120' (state/ab_tests.rs:110-112), which appends a total AWS does not include.

Shared mechanism for cause 1: create_ab_test.rs builds the record with `input.name.clone()` (:289) and reports `record.name.clone()` (:176), and the uniqueness guard is `store.ab_tests.values().any(|held| held.name == input.name)` (:274), so neither the stored value nor the collision test is case-normalised; ListABTests/GetABTest report the same stored string (state/ab_tests.rs:286, summary()).

## Steps to Reproduce
Prerequisite: git_clone, then run_behavioral_suite (it builds and boots the emulator). bedrock-agentcore answers (40/64 checks pass).

1. Create an IAM role assumable by bedrock-agentcore, a gateway (so you have a gatewayArn matching the modeled pattern) and an online evaluation config ARN for the evaluationConfig, as the checks' helpers do.
2. bedrock-agentcore.create_ab_test(name='ProbeAbTestMixedCase', gatewayArn=<gateway arn>, roleArn=<role arn>, evaluationConfig={'perVariantOnlineEvaluationConfig': [{'name': 'C', 'onlineEvaluationConfigArn': <arn>}, {'name': 'T1', 'onlineEvaluationConfigArn': <arn>}]}, variants=[{'name': 'C', 'weight': 50, 'variantConfiguration': {...}}, {'name': 'T1', 'weight': 50, 'variantConfiguration': {...}}], enableOnCreate=False) — observe the 202 response's name is 'ProbeAbTestMixedCase'; AWS reports 'probeabtestmixedcase'. Then get_ab_test(abTestId=<id>) and list_ab_tests(maxResults=50): both report the mixed-case spelling too.
3. Repeat the identical create_ab_test call — expect ConflictException (HTTP 409) with Message "AB test with name 'ProbeAbTestMixedCase' already exists"; observe "An A/B test with the name ProbeAbTestMixedCase already exists".
4. Repeat step 2 with a name differing only in case ('probeabtestmixedcase') — AWS refuses it as a duplicate; observe the emulator creating a second, distinct A/B test.
5. create_ab_test with the same setup but variants weighted 60 and 60 — expect ValidationException (HTTP 400) with Message exactly 'Variant weights must sum to 100'; observe 'Variant weights must sum to 100, but they sum to 120'.
6. Re-replay bedrock-agentcore.create-ab-test-duplicate-name-blocked and bedrock-agentcore.create-abtest-invalid-variant-weights-rejected to confirm the fix.
