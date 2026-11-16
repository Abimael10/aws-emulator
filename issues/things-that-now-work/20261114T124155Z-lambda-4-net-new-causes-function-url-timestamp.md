Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-function-lifecycle-crud
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-function-lifecycle-crud lam-page-list-function-url-configs lambda-code-signing-config-enforcement lambda-function-codesigning-attach-detach-lifecycle lambda-function-codesigning-attach-error-paths
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateFunction|4xx->2xx|$.Type
Issue-Key: lambda|CreateFunctionUrlConfig|2xx->2xx|$.CreationTime
Issue-Key: lambda|ListFunctionUrlConfigs|2xx->2xx|$.FunctionUrlConfigs[*].CreationTime
Issue-Key: lambda|ListFunctionUrlConfigs|2xx->2xx|$.FunctionUrlConfigs[*].LastModifiedTime
Issue-Key: lambda|GetFunctionCodeSigningConfig|2xx->2xx|$
Issue-Related: 20261103T165236Z-lambda-createfunction-skips-package-and-code-signing-validation.md
Issue-Related: 20261116T141500Z-lambda-createfunction-accepts-an-execution-role-lambda-cannot-assume.md
Issue-Commit: 34779bb431f14dc6a8db9e00edffcb539add901f

# lambda: 4 net-new causes — function-URL timestamp format, empty code-signing read, missing role-trust refusal

> _Note: this issue was identified against the code base at commit `34779bb431f14dc6a8db9e00edffcb539add901f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Resolution (2026-11-16)

Causes 2, 3 and 4 are FIXED, and Cause 1 is SPLIT OUT into its own open record —
`issues/things-that-do-not-work/20261116T141500Z-lambda-createfunction-accepts-an-execution-role-lambda-cannot-assume.md`
— so the refusal it asks for stays tracked rather than being closed with this record. That
record carries Cause 1 verbatim, the one-line reading that fixes it, and the 28 in-crate test
expectations plus the cross-service suite that the reversal reaches; the same cluster is also
Cause 1 of `20261116T024510Z-lambda-3-net-new-causes-in-request-validation.md`.

* Causes 2 and 3 — a function URL's `CreationTime` and `LastModifiedTime` now carry the W3C
  NOTE-datetime designator the members' own documentation links, from one renderer the two
  write paths stamp the record with and every read projects
  (`state::function_url::function_url_iso8601`); every other instant member of this service
  keeps the numeric-offset form AWS's own documented example spells. Pinned by
  `tests/all/function_url_instant_form.rs`. The instant ITSELF is a per-run value, so the
  replay still differs on it.
* Cause 4 — `GetFunctionCodeSigningConfig` now names the function it reports on in every 200,
  with the attached configuration's ARN beside it when the function holds one, instead of
  answering an empty body. Pinned by `tests/all/function_code_signing_config_read.rs`. The
  recorded value of `FunctionName` at the four reads of a function holding NO association is
  the recording environment's own (masked in the fixture), so the replay still differs on it.


## Operation
CreateFunction

## Description
Replay of the simparity-wire-aws-v1 corpus against the emulator surfaces FOUR distinct net-new lambda causes in control-plane response payloads and refusals. All four must be fixed and re-replayed before this issue is closed.

Cause 1 of 4: CreateFunction accepts an execution role Lambda cannot assume (fixture lambda-function-lifecycle-crud, step task#7).
AWS: CreateFunction validates the Role before creating the function and refuses a role whose trust policy does not let lambda.amazonaws.com assume it. Smithy: CreateFunction.Role is @required, target RoleArn, pattern ^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$; modeled error InvalidParameterValueException (errorKind client, httpError 400, members Type and message; doc "One of the parameters in the request is not valid"). The recorded response is HTTP 400 with header x-amzn-errortype: InvalidParameterValueException and body {"Type":"User","message":"The role defined for the function cannot be assumed by Lambda."}. That message is entirely fixed text for this error class — it carries no request- or state-derived values at all; "User" is the fixed Type for a caller-caused refusal.
Emulator: answers HTTP 201 and creates the function. $.Type, $.message and the x-amzn-errortype header are all absent, so no refusal is emitted at all.
Why: a client that misconfigures an execution-role trust policy gets a function the emulator reports as created but that AWS would never have accepted; the configuration error is discovered only in production, and negative-path tests that assert the 400 refusal pass vacuously.
Fix: the role-assumability check in the CreateFunction handler's input validation (it must run before the function is persisted) plus the InvalidParameterValueException serializer, which must set the x-amzn-errortype header and the Type/message body members.
Related open issues that do NOT cover this (the replay reports this cluster unowned): 20261029T180026Z-dynamodb-events-lambda-eventbridge-rules-never-invoke-their-lambda-targets-ddb.md (behavioral, bundles a role-trust observation among EventBridge causes) and 20261109T163411Z-cloudformation-iam-lambda-s3-lambda-createfunction-accepts-a-role-arn-that-names.md (a role ARN naming no existing role, a different precondition).
Traces: lambda-function-lifecycle-crud#task#7 (CreateFunction, expected 400, got 201).

Cause 2 of 4: CreateFunctionUrlConfig returns CreationTime in a non-ISO-8601 timezone form (fixture lam-page-list-function-url-configs, step task#5).
AWS: Smithy CreateFunctionUrlConfigResponse.CreationTime is @required, target Timestamp, documented as "When the function URL was created, in ISO-8601 format (YYYY-MM-DDThh:mm:ss.sTZD)" (W3C NOTE-datetime). The TZD component of that profile is either the literal designator Z or an offset written +hh:mm / -hh:mm — always colon-separated. The fixture asserts the shape of this member rather than a literal instant (expected '<shaped-not-asserted>'), i.e. the timestamp text must match the documented serialization; the instant itself is a per-run value and is not a requirement.
Emulator: serializes '2026-11-14T12:35:56.333+0000' — a Java-style four-digit offset with no colon, which is not a valid W3C/ISO-8601 TZD, so the shaped assertion fails.
Why: clients that parse these members with a strict ISO-8601/RFC 3339 parser (the behavior AWS's documented format licenses) throw on every function URL read, so function-URL inventory and age-based logic break.
Fix: the timestamp serializer used by the function-URL state projection for CreateFunctionUrlConfig/GetFunctionUrlConfig/UpdateFunctionUrlConfig/ListFunctionUrlConfigs; it must emit the documented TZD form rather than the function-configuration LastModified style.
Traces: lam-page-list-function-url-configs#task#5 (CreateFunctionUrlConfig 201, $.CreationTime), cluster also covers lam-page-list-function-url-configs#task#6.

Cause 3 of 4: ListFunctionUrlConfigs returns the same non-ISO-8601 form for every listed config's CreationTime and LastModifiedTime (fixture lam-page-list-function-url-configs, step task#7).
AWS: Smithy FunctionUrlConfig.CreationTime and FunctionUrlConfig.LastModifiedTime are both @required, target Timestamp, each documented "in ISO-8601 format (YYYY-MM-DDThh:mm:ss.sTZD)"; ListFunctionUrlConfigs.FunctionUrlConfigs is a @required list of that shape. Both members are shaped assertions in the fixture, not literal instants.
Emulator: both members come back as '2026-11-14T12:35:56.333+0000' for the listed entry — the same colon-less offset, so the list projection diverges on both members.
Why: the paging/list path is what inventory tooling reads, so every entry is unparseable by a strict ISO-8601 client even when the single-config read is fixed; the two members must be fixed together with Cause 2 but are a separate serialization site (the list projection of FunctionUrlConfig).
Fix: the FunctionUrlConfig list projection in the ListFunctionUrlConfigs handler/serializer.
Traces: lam-page-list-function-url-configs#task#7 (ListFunctionUrlConfigs 200, $.FunctionUrlConfigs[0].CreationTime and $.FunctionUrlConfigs[0].LastModifiedTime), cluster also covers lam-page-list-function-url-configs#task#8.

Cause 4 of 4: GetFunctionCodeSigningConfig answers 200 with an empty JSON object, dropping both required members (fixture lambda-code-signing-config-enforcement, step task#13).
AWS: Smithy GetFunctionCodeSigningConfigResponse has exactly two members, both @required: CodeSigningConfigArn (target CodeSigningConfigArn, pattern ^arn:(aws[a-zA-Z-]*)?:lambda:<region>:<account>:code-signing-config:csc-[a-z0-9]{17}$) and FunctionName (target FunctionName). GET /2020-06-30/functions/{FunctionName}/code-signing-config returns 200 with both members populated from the function's attached code-signing configuration; the recorded response carries both keys (their values are environment-specific and masked in the fixture, so the requirement is their presence and shape, not the recorded ARN or name).
Emulator: answers HTTP 200 with body {} — the verdict is empty_body, neither required member is serialized.
Why: a client cannot tell from the emulator whether a function has a code-signing configuration attached, and SDKs that deserialize required members fail or surface nulls; attach/detach lifecycles and code-signing enforcement checks that read back the attached ARN cannot be verified at all.
Fix: the GetFunctionCodeSigningConfig handler must project the function's attached code-signing-config ARN and the resolved function name from state into the response (handler + response serializer in the lambda code-signing path).
Related open issues that do NOT cover this (cluster reported unowned): 20261103T165236Z-lambda-createfunction-skips-package-and-code-signing-validation.md and 20261026T070343Z-lambda-code-signing-is-not-enforced-updatefunctioncode-and.md both concern enforcement, not this read's payload.
Traces: lambda-code-signing-config-enforcement#task#13; cluster also covers lambda-function-codesigning-attach-detach-lifecycle#task#8, lambda-function-codesigning-attach-error-paths#task#5 and lambda-function-codesigning-attach-error-paths#task#7.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, bootstrap_signal false, steps_total 1828, steps_passed 1688, steps_mismatched 137, score_pct 92.3, divergence_count 57 (53 already owned by open issues).

Cause 1 — fixture lambda-function-lifecycle-crud step task#7, operation CreateFunction, verdict mismatch:
  expected: status 400, header x-amzn-errortype: InvalidParameterValueException, body $.Type = 'User', $.message = 'The role defined for the function cannot be assumed by Lambda.'
  actual:   status 201; $.Type missing, $.message missing, x-amzn-errortype header missing (function created).

Cause 2 — fixture lam-page-list-function-url-configs step task#5, operation CreateFunctionUrlConfig, verdict mismatch, status 201 -> 201:
  $.CreationTime: value_mismatch (expected '<shaped-not-asserted>' ISO-8601 YYYY-MM-DDThh:mm:ss.sTZD, got '2026-11-14T12:35:56.333+0000')
  affected: lam-page-list-function-url-configs#task#5, lam-page-list-function-url-configs#task#6

Cause 3 — fixture lam-page-list-function-url-configs step task#7, operation ListFunctionUrlConfigs, verdict mismatch, status 200 -> 200:
  $.FunctionUrlConfigs[0].CreationTime: value_mismatch (expected '<shaped-not-asserted>', got '2026-11-14T12:35:56.333+0000')
  $.FunctionUrlConfigs[0].LastModifiedTime: value_mismatch (expected '<shaped-not-asserted>', got '2026-11-14T12:35:56.333+0000')
  affected: lam-page-list-function-url-configs#task#7, lam-page-list-function-url-configs#task#8

Cause 4 — fixture lambda-code-signing-config-enforcement step task#13, operation GetFunctionCodeSigningConfig, verdict mismatch, status 200 -> 200:
  $: empty_body (expected a body carrying CodeSigningConfigArn and FunctionName, got {})
  affected: lambda-code-signing-config-enforcement#task#13, lambda-function-codesigning-attach-detach-lifecycle#task#8, lambda-function-codesigning-attach-error-paths#task#5, lambda-function-codesigning-attach-error-paths#task#7

Model references used: Smithy model services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json — CreateFunction (Role @required RoleArn; error InvalidParameterValueException httpError 400 with Type/message), CreateFunctionUrlConfigResponse.CreationTime and FunctionUrlConfig.CreationTime/LastModifiedTime (@required Timestamp, documented ISO-8601 YYYY-MM-DDThh:mm:ss.sTZD), GetFunctionCodeSigningConfigResponse (CodeSigningConfigArn and FunctionName both @required).

## Steps to Reproduce
1. git clone the project repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus) and look for the four unowned lambda clusters below.

Cause 1: replay fixture lambda-function-lifecycle-crud up to step task#7 — POST /2015-03-31/functions with a Role ARN whose trust policy does not permit lambda.amazonaws.com to assume it. Expect HTTP 400, x-amzn-errortype: InvalidParameterValueException, body {"Type":"User","message":"The role defined for the function cannot be assumed by Lambda."}; observe HTTP 201 and a created function.

Cause 2: replay fixture lam-page-list-function-url-configs to step task#5 — POST /2021-10-31/functions/{FunctionName}/url with AuthType set. Inspect $.CreationTime in the 201 body: it must match the documented ISO-8601 TZD form (designator Z or +hh:mm); observe '...+0000'.

Cause 3: continue the same fixture to step task#7 — GET /2021-10-31/functions/{FunctionName}/urls. Inspect $.FunctionUrlConfigs[0].CreationTime and .LastModifiedTime: same colon-less offset.

Cause 4: replay fixture lambda-code-signing-config-enforcement to step task#13 — attach a code-signing configuration to a function, then GET /2020-06-30/functions/{FunctionName}/code-signing-config. Expect 200 with CodeSigningConfigArn and FunctionName; observe 200 with body {}. The same empty body appears at lambda-function-codesigning-attach-detach-lifecycle#task#8 and lambda-function-codesigning-attach-error-paths#task#5 / #task#7.
