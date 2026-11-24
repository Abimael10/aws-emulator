Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-notify-code-configuration-channels-and-tags
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-notify-code-configuration-channels-and-tags endusermessaging-notify-code-configuration-validation-and-idempotency endusermessaging-notify-code-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.notifyCodeConfigurationArn
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.notifyCodeConfigurationId
Issue-Commit: 83417ec3dff2f3efed8690d96b9cf1f73f5857f5

# endusermessaging CreateNotifyCodeConfiguration mints a notifyCodeConfigurationId whose form the recorded responses do not carry

> _Note: this issue was identified against the code base at commit `83417ec3dff2f3efed8690d96b9cf1f73f5857f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNotifyCodeConfiguration

## Description
CreateNotifyCodeConfiguration answers HTTP 200 with a well-formed body, but the system-generated identifier it mints for the resource does not satisfy the shape the recorded responses carry for `notifyCodeConfiguration.notifyCodeConfigurationId`, and the ARN the emulator builds from that identifier inherits the divergence. 2 causes, both rooted in the same minting rule.

Cause 1 of 2 - fixture `endusermessaging-notify-code-configuration-channels-and-tags`, step `task#1` (same cluster: `endusermessaging-notify-code-configuration-validation-and-idempotency#task#2`): BOTH identifier members of the created resource diverge.
  AWS: recorded behavior only. Smithy says the identifier is output-only and opaque - `NotifyCodeConfiguration$notifyCodeConfigurationId` targets `NotifyCodeConfigurationId` (@length 1..64, pattern ^[A-Za-z0-9_-]+$, doc: "Bare system-generated NotifyCodeConfiguration identifier (output-only) ... Identifiers are opaque tokens; clients must not assume a specific length or format"), and `notifyCodeConfigurationArn` targets `AmazonResourceName` (@length 20..256, pattern ^arn:[A-Za-z0-9_:/-]+$). The model therefore does not pin a concrete form; the corpus does: the recorded step asserts these two members by SHAPE (the step records them as '<shaped-not-asserted>', i.e. compared as a form rather than as a literal value, because the concrete id/ARN are per-run values), and the form the recording carries for this resource's identifier is NOT the one the emulator mints.
  Emulator: returns 200 with `$.notifyCodeConfiguration.notifyCodeConfigurationId` = an `ncc-` prefix plus a 17-character body drawn from the lowercase hexadecimal digits only (replay saw `ncc-e9af02a9e1451d583`), and `$.notifyCodeConfiguration.notifyCodeConfigurationArn` = `arn:aws:end-user-messaging:<region>:<account>:notify-code-configuration/<that same id>`. Both members are reported as value_mismatch against the step's shape expectation.
  Hypothesis: the notify-code-configuration identifier is minted with the brand-profile convention rather than its own - the width/alphabet of the body is shared with `bp-` ids (`MINTED_ID_BODY_LENGTH` is literally defined as `BRAND_PROFILE_ID_BODY_LENGTH`), and `bp-`+17-hex IS accepted by the corpus elsewhere, so the divergence is specific to this resource's identifier form, not to the shape check in general. The requirement is that the minted identifier have the form the recorded notify-code-configuration responses carry (prefix, body width and body alphabet), and that the ARN embed that identifier unchanged in its `notify-code-configuration/<id>` resource field.
  Why: a client that round-trips the identifier is given a token of the wrong form - anything that validates, stores at a fixed width, or regex-matches the id or the ARN's resource field (`NotifyCodeConfigurationIdOrArn` on GetNotifyCodeConfiguration / UpdateNotifyCodeConfiguration / DeleteNotifyCodeConfiguration / SendNotifyCodeVerification, plus `resourceArn` on the tag operations) accepts the emulator's value where it would reject AWS's, or vice versa, so id-shape handling cannot be exercised against the emulator at all. No status or error code diverges here; the resource body does.
  Fix: the service's id minting in state (the notify-code-configuration id minter and the shared minted-id width/alphabet it draws from), plus the ARN builder that embeds the id. Nothing in the handler needs to change beyond consuming the corrected minter.
  Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#1 (representative), endusermessaging-notify-code-configuration-validation-and-idempotency#task#2.

Cause 2 of 2 - fixture `endusermessaging-notify-code-configuration-lifecycle`, step `task#1`: the identifier member alone diverges.
  AWS: recorded behavior only, same modeled members as Cause 1; this step shape-asserts `notifyCodeConfiguration.notifyCodeConfigurationId` only.
  Emulator: returns 200 with `$.notifyCodeConfiguration.notifyCodeConfigurationId` = `ncc-` + 17 lowercase hex characters (replay saw `ncc-3ac1913dbb18f18ce`), reported as value_mismatch against that shape expectation while this step's ARN expectation is satisfied - confirming the fault is the identifier's own form and not the ARN template around it.
  Why: the lifecycle flow hands this identifier to the subsequent read/update/delete steps, so every consumer of a created configuration's id receives a token of a form AWS does not issue.
  Fix: the same id minter in state; once the minted form is right both this member and the ARN that embeds it follow.
  Traces: endusermessaging-notify-code-configuration-lifecycle#task#1.

Not covered by any open issue: the open endusermessaging issues all concern brand-profile attributes, brand-profile id-format refusals, tag reads/writes or unimplemented stubs (UntagResource, UpdateBrandProfileFromRegistration); none mentions notify-code-configuration identifiers. Nearest in subject matter is '20261117T222227Z-endusermessaging-brand-profile-id-format-refusal-reports-a' (brand-profile id REFUSAL wording, a different operation and a different member).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator up; /__admin/health 200):

Cluster 1 (2 steps) - CreateNotifyCodeConfiguration, expected 200, actual 200:
  divergence_key: service=endusermessaging|CreateNotifyCodeConfiguration|status=200->200|keys=$.notifyCodeConfiguration.notifyCodeConfigurationArn,$.notifyCodeConfiguration.notifyCodeConfigurationId
  $.notifyCodeConfiguration.notifyCodeConfigurationId: value_mismatch (expected '<shaped-not-asserted>', got 'ncc-e9af02a9e1451d583')
  $.notifyCodeConfiguration.notifyCodeConfigurationArn: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:end-user-messaging:us-west-2:000000000000:notify-code-configuration/ncc-e9af02a9e1451d583')
  affected: endusermessaging-notify-code-configuration-channels-and-tags#task#1, endusermessaging-notify-code-configuration-validation-and-idempotency#task#2

Cluster 2 (1 step) - CreateNotifyCodeConfiguration, expected 200, actual 200:
  divergence_key: service=endusermessaging|CreateNotifyCodeConfiguration|status=200->200|keys=$.notifyCodeConfiguration.notifyCodeConfigurationId
  $.notifyCodeConfiguration.notifyCodeConfigurationId: value_mismatch (expected '<shaped-not-asserted>', got 'ncc-3ac1913dbb18f18ce')
  affected: endusermessaging-notify-code-configuration-lifecycle#task#1

Contrast (passing): the same corpus accepts the brand-profile identifier form the emulator mints (`bp-` + 17 lowercase hex, e.g. bp-cf929ddd3d17772bb appears in passing/other-cause steps), so the shape comparison itself works and only the notify-code-configuration identifier form diverges.

Source of the emulated form (citations only, not a patch):
  services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:68 MINTED_ID_DIGITS = b"0123456789abcdef"
  services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:75 MINTED_ID_BODY_LENGTH = BRAND_PROFILE_ID_BODY_LENGTH (17)
  services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:88-96 minted_id(prefix) -> "{prefix}-{17 chars}"
  services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:259-266 mint_notify_code_configuration_id()
  services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:524-530 notify_code_configuration_arn(region, id)
  services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/create_notify_code_configuration.rs:251-254, 270-273 (mint, build ARN, project the view)

Smithy consulted: CreateNotifyCodeConfiguration (POST /v1/notify-code-configurations, success 200, output member notifyCodeConfiguration required); NotifyCodeConfiguration$notifyCodeConfigurationId -> NotifyCodeConfigurationId @length(1..64) pattern ^[A-Za-z0-9_-]+$; NotifyCodeConfiguration$notifyCodeConfigurationArn -> AmazonResourceName @length(20..256) pattern ^arn:[A-Za-z0-9_:/-]+$; NotifyCodeConfigurationIdOrArn doc ("Identifiers are opaque tokens; clients must not assume a specific length or format").

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout.
2. Replay trace set simparity-wire-aws-v1.
3. Cause 1: replay fixture `endusermessaging-notify-code-configuration-channels-and-tags`, step `task#1` - POST /v1/notify-code-configurations with a notifyCodeConfigurationName plus channelParameters/tags as the fixture records. The response is 200, and the step's shape expectations for `$.notifyCodeConfiguration.notifyCodeConfigurationId` and `$.notifyCodeConfiguration.notifyCodeConfigurationArn` both fail; the emulator returns an id of the form `ncc-` + 17 lowercase hex characters and an ARN embedding it. The same divergence reproduces at `endusermessaging-notify-code-configuration-validation-and-idempotency#task#2`.
4. Cause 2: replay fixture `endusermessaging-notify-code-configuration-lifecycle`, step `task#1` - the same create. The response is 200 and the step's shape expectation for `$.notifyCodeConfiguration.notifyCodeConfigurationId` fails on the same `ncc-` + 17-lowercase-hex form (the ARN expectation passes at this step).
5. After changing the minter, re-replay all three fixture steps named above; a fix must make every one of them pass, not just the representative.
