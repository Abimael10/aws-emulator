Issue-Source: trace-sweep-divergence
Issue-Trace: config-conformance-pack-create-inspect-delete
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: config-conformance-pack-create-inspect-delete
Issue-Shape-Id: com.amazonaws.configservice#StarlingDoveService
Issue-Key: config|DescribeConformancePackStatus|2xx->2xx|$.ConformancePackStatusDetails[*]
Issue-Commit: b346fe5639d35cd9791aa731a7b2fb5c4cb110f2

# REOPENS: config conformance-pack deployment has no failure state, so DescribeConformancePackStatus still reports CREATE_COMPLETE

> _Note: this issue was identified against the code base at commit `b346fe5639d35cd9791aa731a7b2fb5c4cb110f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeConformancePackStatus

## Description
REOPENS 20261112T185622Z-config-describeconformancepackstatus-always-reports-create-complete-never-the.md, which is in the RESOLVED folder but whose replay key still diverges on this replay of simparity-wire-aws-v1 (score 94.0%, emulator_up=true). Because that issue is resolved it no longer owns the failure (the replay reports owned_by=[] for this cluster), so the divergence is filed net-new here. 1 cause.

Cause 1 of 1: the conformance-pack lifecycle in the config emulator has no failure state at all. Every PutConformancePack is settled optimistically as a successful CloudFormation deployment, so DescribeConformancePackStatus can only ever project the CREATE_COMPLETE terminal state (with a synthesised StackArn and no ConformancePackStatusReason) — there is no input that yields CREATE_FAILED or DELETE_FAILED. Fixture config-conformance-pack-create-inspect-delete, step task#3.

AWS: Smithy (com.amazonaws.configservice#StarlingDoveService) models DescribeConformancePackStatus -> ConformancePackStatusDetailsList of ConformancePackStatusDetail{Type (StringWithCharLimit256), ConformancePackName (required), ConformancePackId (required), ConformancePackArn (required), ConformancePackState (required, enum ConformancePackState), StackArn (required, @length 1..2048), ConformancePackStatusReason (optional, @length 0..2000), LastUpdateRequestedTime (required, timestamp), LastUpdateCompletedTime (timestamp)}. The ConformancePackState enum admits CREATE_IN_PROGRESS | CREATE_COMPLETE | CREATE_FAILED | DELETE_IN_PROGRESS | DELETE_FAILED, and its documentation states Config sets the state to CREATE_COMPLETE 'when a conformance pack has been successfully created in your account' and CREATE_FAILED 'when a conformance pack creation failed in your account'; ConformancePackStatusReason is documented as 'The reason of conformance pack creation failure', and LastUpdateCompletedTime as 'Last time when conformation pack creation and update was successful'. So the state is DERIVED from the real outcome of materialising the pack, and a pack whose deployment failed is reported CREATE_FAILED and carries the failure reason. Recorded behavior for this fixture's pack agrees and additionally shows AWS omitting StackArn entirely in that failed state even though the member is modeled required: the recorded detail is Type=CUSTOM, the minted name/id/ARN, ConformancePackState=CREATE_FAILED, a PRESENT ConformancePackStatusReason (asserted as shaped, not by value), LastUpdateRequestedTime and LastUpdateCompletedTime shaped, and NO StackArn member. Recorded status is 200 and no error is involved, so there is no error code or error message requirement in this cause. The requirement is: ConformancePackState == CREATE_FAILED for a pack whose deployment did not succeed, ConformancePackStatusReason present, and StackArn omitted in that state. The pack name, pack id, both ARNs, the reason's text, the stack uuid and the two timestamps are per-run values and are NOT part of the requirement.

Emulator: the same DescribeConformancePackStatus call answers HTTP 200 (status matches) but ConformancePackStatusDetails[0] is fabricated from the PutConformancePack request: ConformancePackState='CREATE_COMPLETE', a synthesised StackArn of the form arn:aws:cloudformation:<region>:<account>:stack/awsconfigconforms-<packName>-<packId>/<uuid>, NO ConformancePackStatusReason member, and LastUpdateCompletedTime set a fixed half second after LastUpdateRequestedTime. The replay reports the whole element as diverging ($.ConformancePackStatusDetails[0]: missing).

Why: polling DescribeConformancePackStatus for a terminal state is the documented way to learn whether a pack deployed. A client is told a pack that failed to deploy is live and complete, so error handling keyed on CREATE_FAILED is never reached and the ConformancePackStatusReason that explains the failure is never delivered — automation concludes its compliance coverage is in place when no rules were deployed. The fabricated StackArn compounds this: it addresses a CloudFormation stack that corresponds to no successful deployment, so a client that follows it to CloudFormation gets a second, inconsistent answer. The invariant LastUpdateCompletedTime (requested + 0.5s) also makes a never-completed pack look completed.

Fix: the conformance-pack state model in the config service's state plus the DescribeConformancePackStatus projection. PutConformancePack must record the actual outcome of materialising the pack's template (the same path that would create the pack's member AWS::Config::ConfigRule resources) and settle the pack at CREATE_FAILED with a ConformancePackStatusReason when that materialisation does not succeed, instead of unconditionally stamping CREATE_COMPLETE; the serializer must then omit StackArn for a pack that has no successfully created stack, emit ConformancePackStatusReason when one is recorded, and leave LastUpdateCompletedTime unset until a deployment actually completes. Related open issues do NOT cover this: 20261119T193246Z-config-lambda-sqs-2-net-new-causes-a--r2.md and 20261122T033830Z-config-sqs-a-configuration-recorder-restarted-with-startconfigurationrecorder.md concern rule invocation and the configuration recorder, and 20261122T163452Z-config-server-assigned-ids-are-minted-as-hyphenated.md explicitly excludes this divergence; none carries this fixture step's replay key.

Traces: config-conformance-pack-create-inspect-delete#task#3 (affected_count 1).

## Evidence
Replay of trace set simparity-wire-aws-v1: 56 fixtures replayed, 282 steps, 265 passed, 17 mismatched, 0 errored, 0 blocked, score_pct 94.0, emulator_up=true, bootstrap_signal=false, divergence_count=5. Four of the five clusters are owned by open issue 20261122T163452Z-config-server-assigned-ids-are-minted-as-hyphenated.md; this is the one unowned cluster.

Divergence cluster: divergence_key 'service=config|DescribeConformancePackStatus|status=200->200|keys=$.ConformancePackStatusDetails[0]', verdict=mismatch, affected_count=1, stub_path='' , owned_by=[].
Fixture config-conformance-pack-create-inspect-delete, step task#3, operation DescribeConformancePackStatus, expected_status 200, actual_status 200 — the divergence is entirely in the body at $.ConformancePackStatusDetails[0].

detail (verbatim from replay):
  $.ConformancePackStatusDetails[0]: missing (expected {'ConformancePackArn': 'arn:aws:config:us-west-2:000000000000:conformance-pack/sim2real-probe-config-cfpack-7ca2ab66/conformance-pack-6jyihvtr', 'ConformancePackId': 'conformance-pack-6jyihvtr', 'ConformancePackName': 'sim2real-probe-config-cfpack-7ca2ab66', 'ConformancePackState': 'CREATE_FAILED', 'ConformancePackStatusReason': '<shaped-not-asserted>', 'LastUpdateCompletedTime': '<shaped-not-asserted>', 'LastUpdateRequestedTime': '<shaped-not-asserted>', 'Type': 'CUSTOM'}, got {'Type': 'CUSTOM', 'ConformancePackName': 'sim2real-probe-config-cfpack-7ca2ab66', 'ConformancePackId': 'conformance-pack-6jyihvtr', 'ConformancePackArn': 'arn:aws:config:us-west-2:000000000000:conformance-pack/sim2real-probe-config-cfpack-7ca2ab66/conformance-pack-6jyihvtr', 'ConformancePackState': 'CREATE_COMPLETE', 'StackArn': 'arn:aws:cloudformation:us-west-2:000000000000:stack/awsconfigconforms-sim2real-probe-config-cfpack-7ca2ab66-conformance-pack-6jyihvtr/566258dd-83ac-4553-9973-ac4b3855eca7', 'LastUpdateRequestedTime': 1795545661.9044852, 'LastUpdateCompletedTime': 1795545662.4044852})

Member-by-member: ConformancePackName, ConformancePackId, ConformancePackArn and Type AGREE. The divergence is (a) ConformancePackState 'CREATE_FAILED' expected vs 'CREATE_COMPLETE' actual, (b) ConformancePackStatusReason present-and-shaped expected vs member absent actual, (c) StackArn absent expected vs a synthesised CloudFormation stack ARN actual. The concrete pack name 'sim2real-probe-config-cfpack-7ca2ab66', pack id 'conformance-pack-6jyihvtr', the stack uuid '566258dd-...' and the epoch timestamps 1795545661.9044852 / 1795545662.4044852 are per-run values filling the template, not requirements.

Model citations (services/service-com-amazonaws-configservice-starlingdoveservice/smithy-model/com-amazonaws-configservice-starlingdoveservice.json):
  ConformancePackState enum values: CREATE_IN_PROGRESS, CREATE_COMPLETE, CREATE_FAILED, DELETE_IN_PROGRESS, DELETE_FAILED.
  ConformancePackStatusDetail$ConformancePackState: required, doc 'Indicates deployment status of conformance pack. Config sets the state of the conformance pack to: CREATE_IN_PROGRESS when a con...' (CREATE_COMPLETE = successfully created, CREATE_FAILED = creation failed).
  ConformancePackStatusDetail$ConformancePackStatusReason: optional, @length 0..2000, doc 'The reason of conformance pack creation failure.'
  ConformancePackStatusDetail$StackArn: required, @length 1..2048, doc 'Amazon Resource Name (ARN) of CloudFormation stack.' — yet the recording omits it in the failed state.
  ConformancePackStatusDetail$LastUpdateCompletedTime: optional timestamp, doc 'Last time when conformation pack creation and update was successful.'

Regression note: issue 20261112T185622Z-config-describeconformancepackstatus-always-reports-create-complete-never-the.md (filed against commit 32b3cf39b49b34d906693dfa43539d42886b562b) carries exactly this replay key and is listed as FIXED, but the emulator built from the current checkout still answers CREATE_COMPLETE with the same synthesised StackArn and the same requested+0.5s LastUpdateCompletedTime, so the behavior is unchanged.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from it, and replay trace set simparity-wire-aws-v1.
2. Inspect fixture 'config-conformance-pack-create-inspect-delete', step 'task#3' (operation DescribeConformancePackStatus): it answers 200 but $.ConformancePackStatusDetails[0] diverges.
3. Equivalently by hand: call PutConformancePack(ConformancePackName=<name>, TemplateBody=<the fixture's pack template>) in a region/account where the pack's deployment cannot succeed (as in the recording), then call DescribeConformancePackStatus(ConformancePackNames=[<name>]).
4. Observe the emulator reports ConformancePackStatusDetails[0].ConformancePackState = 'CREATE_COMPLETE', a synthesised StackArn 'arn:aws:cloudformation:<region>:<account>:stack/awsconfigconforms-<packName>-<packId>/<uuid>', no ConformancePackStatusReason member, and LastUpdateCompletedTime exactly 0.5 s after LastUpdateRequestedTime — and that it stays there permanently.
5. AWS instead settles the same pack at ConformancePackState = 'CREATE_FAILED' with a present ConformancePackStatusReason explaining the failure and NO StackArn member.
6. Confirm the underlying defect: no input to the emulator produces CREATE_FAILED or DELETE_FAILED — the pack lifecycle has no failure state and PutConformancePack never consults a deployment outcome.
7. After the fix, re-replay config-conformance-pack-create-inspect-delete#task#3 (this issue's Issue-Trace/Issue-Step) and confirm it passes.
