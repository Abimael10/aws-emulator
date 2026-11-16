Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-function-lifecycle-crud
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-function-lifecycle-crud lambda-lls-list-filter-and-paging-parameter-mistakes
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateFunction|4xx->2xx|$.message
Issue-Key: lambda|CreateFunction|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: lambda|CreateFunction|4xx->2xx|status
Issue-Key: lambda|ListLayers|4xx->4xx|$.message
Issue-Key: lambda|ListLayerVersions|4xx->4xx|$.message
Issue-Related: 20261114T124155Z-lambda-4-net-new-causes-function-url-timestamp.md
Issue-Commit: 6f0fa83161e30152cf12b521a6456feb5eaff8e6

# lambda: 3 net-new causes in request validation — CreateFunction accepts an unassumable execution role, and the layer-list CompatibleRuntime refusal enumerates the wrong runtime value set

> _Note: this issue was identified against the code base at commit `6f0fa83161e30152cf12b521a6456feb5eaff8e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunction

## Description
Replay of the simparity-wire-aws-v1 wire corpus against the emulator surfaces THREE distinct lambda causes in control-plane input validation that the replay reports as unowned by any open issue. All three must be fixed and re-replayed before this issue is closed.

Cause 1 of 3: CreateFunction accepts an execution role that Lambda cannot assume (fixture lambda-function-lifecycle-crud, step task#7).
AWS: CreateFunction validates the execution role before the function is persisted and refuses a role whose trust policy does not allow lambda.amazonaws.com to assume it. Smithy: CreateFunction.Role is @required, target RoleArn, pattern ^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$; CreateFunction's modeled errors include InvalidParameterValueException (errorKind client, httpError 400, members Type and message, doc "One of the parameters in the request is not valid"); the operation's success code is 201. The recorded response is HTTP 400 with header x-amzn-errortype: InvalidParameterValueException and body {"Type":"User","message":"The role defined for the function cannot be assumed by Lambda."}. That message is entirely fixed text for this error class — it interpolates nothing from the request or from state — and "User" is the fixed Type value AWS uses for a caller-caused refusal.
Emulator: answers HTTP 201 and creates the function. $.Type, $.message and the x-amzn-errortype response header are all absent: no refusal is emitted at all.
Why: a client that misconfigures an execution-role trust policy is told the function was created, when AWS would never have accepted it — the misconfiguration is only discovered at first invoke in a real account, and any negative-path test that asserts the 400 refusal passes vacuously against the emulator.
Fix: the role-assumability check in the CreateFunction handler's input validation (it must run before the function is written to state), plus the InvalidParameterValueException serializer, which must set the x-amzn-errortype header and the Type/message body members.
Note on an existing issue: 20261114T124155Z-lambda-4-net-new-causes-function-url-timestamp.md names this same fixture step as its Cause 1, but the current replay still reports this cluster unowned and still diverging, so it is re-filed here rather than dropped; 20261103T165236Z-lambda-createfunction-skips-package-and-code-signing-validation.md covers package/code-signing validation, not role trust.
Traces: lambda-function-lifecycle-crud#task#7 (CreateFunction, expected 400, got 201).

Cause 2 of 3: ListLayers' CompatibleRuntime validation refusal lists only the modeled public Runtime enum, not AWS's accepted runtime value set (fixture lambda-lls-list-filter-and-paging-parameter-mistakes, step task#1).
AWS: Smithy ListLayers.CompatibleRuntime is an httpQuery parameter targeting the enum Runtime; the operation's modeled client error is InvalidParameterValueException (httpError 400). For a query value outside the accepted set AWS answers HTTP 400 with the standard constraint message "1 validation error detected: Value '<value>' at '<memberName>' failed to satisfy constraint: Member must satisfy enum value set: [<accepted values>]". In that template the fixed text is everything outside the brackets and quotes; the variable parts are the rejected value (taken verbatim from the request query string, here a value differing from a legal runtime only in case) and the member name 'compatibleRuntime' (the lower-camel wire name of the parameter). The bracketed set is NOT a per-run value: it is AWS's accepted-value list for Runtime, and the recording shows it contains 63 entries — the 51 values the Smithy enum publishes PLUS 12 retired/internal values AWS still accepts in this list: ruby2.6, nasa, python2.7-greengrass, custom, go1.9, byol, nodejs8.x, nodejs8.9, python3.4 and the three Greengrass runtime ARNs arn:aws:greengrass:::runtime/function/executable, arn:aws-cn:greengrass:::runtime/function/executable and arn:aws-us-gov:greengrass:::runtime/function/executable. The recording also shows AWS's own enumeration order for that set, which is not the model's declaration order.
Emulator: answers HTTP 400 with the correct fixed text, the correct rejected value and the correct member name, but the bracketed set is exactly the modeled Runtime enum in model declaration order (51 values, beginning 'nodejs, nodejs4.3, nodejs6.10, ...'), so the message differs from AWS's for every rejected runtime: the 12 values above are missing and the ordering differs.
Why: the enum value set is part of the error message a client sees and logs; tooling and tests that assert on the refusal text, and users who read the list to discover which runtimes are accepted, get a list that disagrees with AWS — and the 12 omitted values are runtimes AWS accepts, so the message also implies filters that AWS honours would be rejected.
Fix: the validation-message builder for the Runtime enum in the layer-listing path — it must render the service's accepted-value list (its own Runtime value table, including the retired and Greengrass entries, in AWS's order) rather than reflecting over the modeled enum.
Traces: lambda-lls-list-filter-and-paging-parameter-mistakes#task#1 (ListLayers, status 400 -> 400, $.message value_mismatch).

Cause 3 of 3: ListLayerVersions' CompatibleRuntime refusal carries the same wrong value set (fixture lambda-lls-list-filter-and-paging-parameter-mistakes, step task#5).
AWS: Smithy ListLayerVersions.CompatibleRuntime is likewise an httpQuery parameter targeting Runtime, with InvalidParameterValueException (httpError 400) modeled; the refusal uses the identical message template, with the rejected value from the request (here an all-upper-case spelling of a legal runtime) and member name 'compatibleRuntime', and the same 63-value accepted set described in Cause 2.
Emulator: answers HTTP 400 with the same 51-value modeled-enum list, so this second entry point diverges on $.message in exactly the same way — it is listed separately because ListLayerVersions is a distinct handler/validation site and a fix to the layers path alone would leave it diverging.
Why: same client impact as Cause 2, on the per-layer version listing that paging and layer-compatibility tooling uses.
Fix: the same shared Runtime accepted-value table must back the ListLayerVersions query-parameter validation message, so both layer-list operations render one AWS-accurate set.
Traces: lambda-lls-list-filter-and-paging-parameter-mistakes#task#5 (ListLayerVersions, status 400 -> 400, $.message value_mismatch).

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, bootstrap_signal false, fixtures_replayed 171, steps_total 1828, steps_passed 1790, steps_mismatched 38, steps_errored 0, steps_blocked 0, score_pct 97.9, divergence_count 22 (19 clusters already owned by open issues, 1 not_implemented filed as a stub).

Cause 1 — fixture lambda-function-lifecycle-crud, step task#7, operation CreateFunction, verdict mismatch, divergence_key service=lambda|CreateFunction|status=400->201|keys=$.Type,$.message,$headers.x-amzn-errortype,status:
  expected: status 400; header x-amzn-errortype: InvalidParameterValueException; body $.Type = 'User', $.message = 'The role defined for the function cannot be assumed by Lambda.'
  actual:   status 201; $.Type missing, $.message missing, x-amzn-errortype header missing (the function is created).
  affected_count 1; owned_by: (none reported by the replay).

Cause 2 — fixture lambda-lls-list-filter-and-paging-parameter-mistakes, step task#1, operation ListLayers, verdict mismatch, status 400 -> 400, divergence_key service=lambda|ListLayers|status=400->400|keys=$.message:
  expected $.message: "1 validation error detected: Value 'Nodejs20.x' at 'compatibleRuntime' failed to satisfy constraint: Member must satisfy enum value set: [ruby2.6, dotnetcore1.0, python3.7, nodejs8.10, nasa, ruby2.7, python2.7-greengrass, dotnetcore2.0, python3.8, java21, java8.al2023, dotnet6, dotnetcore2.1, python3.9, java11, nodejs6.10, provided, dotnetcore3.1, dotnet8, arn:aws-cn:greengrass:::runtime/function/executable, java25, java17, nodejs, nodejs4.3, java8.al2, java17.al2023, arn:aws-us-gov:greengrass:::runtime/function/executable, custom, go1.x, dotnet10, nodejs20.x, go1.9, byol, nodejs10.x, provided.al2023, nodejs22.x, java11.al2023, python3.10, java8, nodejs12.x, python3.11, nodejs24.x, nodejs8.x, python3.12, nodejs14.x, nodejs8.9, nodejs26.x, python3.13, python3.14, nodejs16.x, python3.15, provided.al2, ruby4.0, nodejs4.3-edge, nodejs18.x, arn:aws:greengrass:::runtime/function/executable, ruby3.2, python3.4, ruby3.3, ruby3.4, ruby2.5, python3.6, python2.7]" (63 values)
  actual   $.message: same fixed text and same rejected value/member name, but the set is "[nodejs, nodejs4.3, nodejs6.10, nodejs8.10, nodejs10.x, nodejs12.x, nodejs14.x, nodejs16.x, nodejs18.x, nodejs20.x, nodejs22.x, nodejs24.x, java8, java8.al2, java11, java17, java21, java25, python2.7, python3.6, python3.7, python3.8, python3.9, python3.10, python3.11, python3.12, python3.13, python3.14, dotnetcore1.0, dotnetcore2.0, dotnetcore2.1, dotnetcore3.1, dotnet6, dotnet8, dotnet10, nodejs4.3-edge, go1.x, ruby2.5, ruby2.7, ruby3.2, ruby3.3, ruby3.4, ruby4.0, provided, provided.al2, provided.al2023, nodejs26.x, python3.15, java8.al2023, java11.al2023, java17.al2023]" — the 51 modeled Runtime enum values, in model declaration order.
  Set difference (expected minus actual, 12 values): ruby2.6, nasa, python2.7-greengrass, custom, go1.9, byol, nodejs8.x, nodejs8.9, python3.4, arn:aws:greengrass:::runtime/function/executable, arn:aws-cn:greengrass:::runtime/function/executable, arn:aws-us-gov:greengrass:::runtime/function/executable. No value present in the emulator's list is absent from AWS's.
  affected_count 1; owned_by: (none reported by the replay).

Cause 3 — fixture lambda-lls-list-filter-and-paging-parameter-mistakes, step task#5, operation ListLayerVersions, verdict mismatch, status 400 -> 400, divergence_key service=lambda|ListLayerVersions|status=400->400|keys=$.message: identical divergence with the rejected value 'NODEJS20.X' at 'compatibleRuntime' — expected AWS's 63-value set, got the same 51-value modeled-enum list. affected_count 1; owned_by: (none reported by the replay).

Model references used (services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json): CreateFunction (Role @required target RoleArn with the IAM-role ARN pattern; success code 201; errors include InvalidParameterValueException), InvalidParameterValueException (errorKind client, httpError 400, members Type and message, doc "One of the parameters in the request is not valid"), ListLayers and ListLayerVersions (CompatibleRuntime httpQuery targeting enum Runtime; InvalidParameterValueException httpError 400), and the Runtime enum, whose 51 published values match the emulator's rendered list exactly.

## Steps to Reproduce
1. git clone the project repository and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus) and look for the three unowned lambda clusters below.

Cause 1: replay fixture lambda-function-lifecycle-crud up to step task#7 — POST /2015-03-31/functions with a well-formed Role ARN whose trust policy does not permit lambda.amazonaws.com to assume the role. Expect HTTP 400 with header x-amzn-errortype: InvalidParameterValueException and body {"Type":"User","message":"The role defined for the function cannot be assumed by Lambda."}; observe HTTP 201 and a created function (no Type, no message, no errortype header).

Cause 2: replay fixture lambda-lls-list-filter-and-paging-parameter-mistakes to step task#1 — GET /2018-10-31/layers?CompatibleRuntime=Nodejs20.x (a value that differs from the legal 'nodejs20.x' only in case). Both AWS and the emulator answer HTTP 400; compare $.message. The bracketed enum value set must be the service's accepted Runtime list (63 entries, including ruby2.6, nasa, python2.7-greengrass, custom, go1.9, byol, nodejs8.x, nodejs8.9, python3.4 and the three Greengrass runtime ARNs, in AWS's order); observe the 51-value modeled enum in model order.

Cause 3: continue the same fixture to step task#5 — GET /2018-10-31/layers/{LayerName}/versions?CompatibleRuntime=NODEJS20.X. Same comparison on $.message: the rejected value and member name 'compatibleRuntime' are right, the enum value set is the 51-value modeled list again.

3. Re-replay all three steps after fixing: both layer-list sites must render one shared accepted-value table, and CreateFunction must refuse the unassumable role with the 400 above.
