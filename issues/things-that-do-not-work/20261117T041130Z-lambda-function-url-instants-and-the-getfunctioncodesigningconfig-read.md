Issue-Source: trace-sweep-divergence
Issue-Trace: lam-page-list-function-url-configs
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lam-page-list-function-url-configs lambda-code-signing-config-enforcement lambda-function-codesigning-attach-detach-lifecycle lambda-function-codesigning-attach-error-paths
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateFunctionUrlConfig|2xx->2xx|$.CreationTime
Issue-Key: lambda|ListFunctionUrlConfigs|2xx->2xx|$.FunctionUrlConfigs[*].CreationTime
Issue-Key: lambda|ListFunctionUrlConfigs|2xx->2xx|$.FunctionUrlConfigs[*].LastModifiedTime
Issue-Key: lambda|GetFunctionCodeSigningConfig|2xx->2xx|$.FunctionName
Issue-Commit: d66b49b5d20b841ed555a4c1167817e25b409ab6

# lambda: function-URL instants and the GetFunctionCodeSigningConfig read still diverge from the recorded payloads (3 causes)

> _Note: this issue was identified against the code base at commit `d66b49b5d20b841ed555a4c1167817e25b409ab6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunctionUrlConfig

## Description
Replay of the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout surfaces THREE net-new, unowned lambda clusters, all of them in control-plane RESPONSE PAYLOAD projection (no status or error-code divergence in any of them). All three must be fixed and the corpus re-replayed before this issue is closed.

Two of them are RECURRENCES of causes a resolved record declared fixed: 20261114T124155Z-lambda-4-net-new-causes-function-url-timestamp.md (resolution note dated 2026-11-16) closed its Causes 2/3 (function-URL instant form) and its Cause 4 (the code-signing read's payload) on the reading that the only residual difference left at those steps was a per-run value. The replay contradicts that reading: both members at issue are SHAPE assertions in the fixture (expected '<shaped-not-asserted>') or a literal absence (expected None), neither of which a per-run instant or a per-run name can fail. A resolved record covers nothing, so these are filed here as net-new.

=== Cause 1 of 3: CreateFunctionUrlConfig renders CreationTime at a narrower fractional-second width than AWS (fixture lam-page-list-function-url-configs, step task#5) ===
AWS: Smithy CreateFunctionUrlConfigResponse.CreationTime is @required, target com.amazonaws.lambda#Timestamp (a string, @length 0..100, pattern ^.*$), documented "When the function URL was created, in ISO-8601 format (YYYY-MM-DDThh:mm:ss.sTZD)" linking the W3C NOTE-datetime profile; the operation answers HTTP 201 (http {method POST, uri /2021-10-31/functions/{FunctionName}/url, code 201}). Recorded behavior: the fixture asserts this member's SHAPE and not its instant (expected '<shaped-not-asserted>'), so the clock reading is explicitly NOT a requirement while the text's form is. AWS's own documented example of this member for this operation is of the form 2022-04-26T18:33:20.230000Z -- the UTC designator Z after a SIX-digit (microsecond) fractional-second field.
Emulator: serializes the designator form at a fixed THREE-digit (millisecond) fractional field -- '2026-11-17T04:04:48.837Z' -- and the shaped assertion rejects it. The designator itself is now right (that part of the earlier fix landed); what is left is the width of the fractional field, which this crate fixes deliberately: services/service-com-amazonaws-lambda-awsgirapiservice/src/state/function_url.rs:573 function_url_iso8601 delegates to state::designated_iso8601, and the module's own test at function_url.rs:620 asserts the rendering equals the width of "2026-01-01T00:00:00.000Z".
Hypothesis: the sole remaining shape difference is that fractional width (three digits where AWS renders six); the recorded literal is masked, so the width is read from AWS's documented example for this member and from the fact that the designator-and-separator form otherwise already matches.
Why: a client that round-trips or compares these texts against instants it obtained from real AWS (or that asserts the documented serialization of the member) sees a different timestamp grammar from every function-URL read, and any consumer keying on the member's exact text form -- caches, golden-file assertions, lexicographic ordering against values captured from AWS -- disagrees with the service it is emulating.
Fix: the function-URL instant renderer the two write paths stamp records with and every read projects -- state::function_url::function_url_iso8601 and the shared state::designated_iso8601 width it borrows (service state, not the serializer: the text is stored).
Traces: lam-page-list-function-url-configs#task#5 (CreateFunctionUrlConfig, 201 -> 201, $.CreationTime); the cluster also covers lam-page-list-function-url-configs#task#6.

=== Cause 2 of 3: ListFunctionUrlConfigs projects the same narrow form for every listed entry's CreationTime and LastModifiedTime (fixture lam-page-list-function-url-configs, step task#7) ===
AWS: Smithy FunctionUrlConfig.CreationTime and FunctionUrlConfig.LastModifiedTime are both @required, target Timestamp, each documented "in ISO-8601 format (YYYY-MM-DDThh:mm:ss.sTZD)" (W3C NOTE-datetime); ListFunctionUrlConfigs.FunctionUrlConfigs is a @required list of FunctionUrlConfig and the operation answers 200 (GET /2021-10-31/functions/{FunctionName}/urls). Recorded behavior: both members are shape assertions in the fixture, not literal instants.
Emulator: both members of the listed entry come back as '2026-11-17T04:04:48.837Z' -- the three-digit fractional field of Cause 1 -- so the list projection diverges on BOTH members.
Why: the list path is what function-URL inventory tooling reads, so every entry in every page carries the diverging form even once the single-configuration read is corrected; and LastModifiedTime is only exposed here and on the single-config read, so this is a second affected member rather than a repeat of the first.
Fix: same renderer as Cause 1, reached through the FunctionUrlConfig list projection in the ListFunctionUrlConfigs handler; the two must agree, which is why the stored-text design is the right place to change.
Traces: lam-page-list-function-url-configs#task#7 (ListFunctionUrlConfigs, 200 -> 200, $.FunctionUrlConfigs[0].CreationTime and $.FunctionUrlConfigs[0].LastModifiedTime); the cluster also covers lam-page-list-function-url-configs#task#8.

=== Cause 3 of 3: GetFunctionCodeSigningConfig names a FunctionName the recorded 200 does not carry (fixture lambda-code-signing-config-enforcement, step task#13) ===
AWS: Smithy GetFunctionCodeSigningConfigResponse has exactly two members, CodeSigningConfigArn (target CodeSigningConfigArn, pattern ^arn:(aws[a-zA-Z-]*)?:lambda:<region>:<account>:code-signing-config:csc-[a-z0-9]{17}$) and FunctionName (target FunctionName), both marked @required; GET /2020-06-30/functions/{FunctionName}/code-signing-config answers 200 (readonly), with ResourceNotFoundException/CodeSigningConfigNotFoundException at 404 for the absent cases. Recorded behavior only for what the 200 carries at these four steps: the recorded response carries NO FunctionName member at all (expected None) while CodeSigningConfigArn does not diverge, i.e. AWS answers this read without naming the function back. Hypothesis: these four steps read a function that holds no code-signing association, and AWS answers such a read with an empty 200 body rather than echoing the function -- which is why the required-member modelling and the recording disagree.
Emulator: answers 200 and ALWAYS includes FunctionName, echoing the resolved function name from the request path (the member added by the earlier fix pinned in tests/all/function_code_signing_config_read.rs), so the payload carries a member the recorded response does not: $.FunctionName type_mismatch (expected None, got the requested function's name).
Why: a client cannot tell an attached code-signing configuration from no association by the response's content any more -- any consumer that treats a non-empty body (or the presence of FunctionName) as evidence that a configuration is attached gets a false positive on every function, and strict deserializers see a member AWS never sent. This is the opposite failure from the empty-body one that was fixed: the read now over-reports instead of under-reporting.
Fix: the GetFunctionCodeSigningConfig handler's response projection in the lambda code-signing path -- project only the members the function's attached configuration actually provides, so a read of a function with no association carries what AWS's recorded 200 carries.
Traces: lambda-code-signing-config-enforcement#task#13; the cluster also covers lambda-function-codesigning-attach-detach-lifecycle#task#8, lambda-function-codesigning-attach-error-paths#task#5 and lambda-function-codesigning-attach-error-paths#task#7.

Related open issues that do NOT cover any of these three (the replay reports all three clusters unowned): 20261116T141500Z-lambda-createfunction-accepts-an-execution-role-lambda-cannot-assume.md (the role-trust refusal split out of the resolved record above), 20261026T070343Z-lambda-code-signing-is-not-enforced-updatefunctioncode-and.md and 20261109T080534Z-lambda-updatefunctioncode-ignores-an-attached-enforce-codesigningconfig-a.md (code-signing ENFORCEMENT, not this read's payload), and the function-URL issues 20261112T074216Z-lambda-rejects-valid-container-image-createfunction-and-returns.md and 20261109T152222Z-apigateway-lambda-function-urls-are-published-on-lambda.md (the announced URL host and the data plane, not the instants' form).

## Evidence
Replay summary (corpus simparity-wire-aws-v1): emulator_up true, bootstrap_signal false, steps_total 1828, steps_passed 1790, steps_mismatched 38, steps_errored 0, steps_blocked 0, score_pct 97.9, divergence_count 22 -- 19 of the 22 clusters are owned by open issues or blocked by an unimplemented-operation stub; the three below are unowned.

Cause 1 -- fixture lam-page-list-function-url-configs step task#5, operation CreateFunctionUrlConfig, verdict mismatch, status 201 -> 201,
divergence_key `service=lambda|CreateFunctionUrlConfig|status=201->201|keys=$.CreationTime`:
  expected: $.CreationTime = '<shaped-not-asserted>' (the member's documented ISO-8601 YYYY-MM-DDThh:mm:ss.sTZD form; AWS's documented example for this member: 2022-04-26T18:33:20.230000Z)
  actual:   $.CreationTime = '2026-11-17T04:04:48.837Z' (designator Z, three-digit fractional field)
  affected: lam-page-list-function-url-configs#task#5, lam-page-list-function-url-configs#task#6 (affected_count 2)

Cause 2 -- fixture lam-page-list-function-url-configs step task#7, operation ListFunctionUrlConfigs, verdict mismatch, status 200 -> 200,
divergence_key `service=lambda|ListFunctionUrlConfigs|status=200->200|keys=$.FunctionUrlConfigs[0].CreationTime,$.FunctionUrlConfigs[0].LastModifiedTime`:
  $.FunctionUrlConfigs[0].CreationTime: value_mismatch (expected '<shaped-not-asserted>', got '2026-11-17T04:04:48.837Z')
  $.FunctionUrlConfigs[0].LastModifiedTime: value_mismatch (expected '<shaped-not-asserted>', got '2026-11-17T04:04:48.837Z')
  affected: lam-page-list-function-url-configs#task#7, lam-page-list-function-url-configs#task#8 (affected_count 2)

Cause 3 -- fixture lambda-code-signing-config-enforcement step task#13, operation GetFunctionCodeSigningConfig, verdict mismatch, status 200 -> 200,
divergence_key `service=lambda|GetFunctionCodeSigningConfig|status=200->200|keys=$.FunctionName`:
  $.FunctionName: type_mismatch (expected None -- the recorded 200 carries no such member -- got the requested function's name, e.g. the probe function named in the request path)
  affected: lambda-code-signing-config-enforcement#task#13, lambda-function-codesigning-attach-detach-lifecycle#task#8, lambda-function-codesigning-attach-error-paths#task#5, lambda-function-codesigning-attach-error-paths#task#7 (affected_count 4)

In-repo references read while grounding this: services/service-com-amazonaws-lambda-awsgirapiservice/src/state/function_url.rs:544-575 (FUNCTION_URL_UTC_DESIGNATOR and function_url_iso8601 -- the ONE rendering both write paths store and every read projects) and :620 (the test asserting the three-digit fixed width). Model references: services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json -- CreateFunctionUrlConfig/ListFunctionUrlConfigs/GetFunctionCodeSigningConfig operation signatures, FunctionUrlConfig.CreationTime/LastModifiedTime docs, com.amazonaws.lambda#Timestamp, GetFunctionCodeSigningConfigResponse members.

## Steps to Reproduce
1. git_clone the project repo and boot the emulator built from that checkout.
2. Call replay_wire_corpus() on corpus simparity-wire-aws-v1 and look for the three clusters with an empty owned_by (service=lambda).

Cause 1: replay fixture lam-page-list-function-url-configs to step task#5 -- POST /2021-10-31/functions/{FunctionName}/url with AuthType set. Inspect $.CreationTime in the 201 body: it must match the member's recorded ISO-8601 form (designator Z with AWS's microsecond fractional field, as in 2022-04-26T18:33:20.230000Z); observe a three-digit fractional field such as '2026-11-17T04:04:48.837Z'. Step task#6 of the same fixture shows it on the single-config read.

Cause 2: continue the same fixture to step task#7 -- GET /2021-10-31/functions/{FunctionName}/urls -- and inspect $.FunctionUrlConfigs[0].CreationTime and .LastModifiedTime: both carry the same narrow form. Step task#8 pages the same listing.

Cause 3: replay fixture lambda-code-signing-config-enforcement to step task#13 -- GET /2020-06-30/functions/{FunctionName}/code-signing-config for the function under test. The recorded 200 carries no FunctionName member; observe a 200 whose body names the function from the request path. The same extra member appears at lambda-function-codesigning-attach-detach-lifecycle#task#8 and lambda-function-codesigning-attach-error-paths#task#5 / #task#7.

3. Re-replay all of lam-page-list-function-url-configs, lambda-code-signing-config-enforcement, lambda-function-codesigning-attach-detach-lifecycle and lambda-function-codesigning-attach-error-paths after any fix: all three clusters must pass, not just the representative step.
