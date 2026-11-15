Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-notify-code-configuration-channels-and-tags
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-notify-code-configuration-channels-and-tags endusermessaging-notify-code-configuration-validation-and-idempotency endusermessaging-notify-code-configuration-lifecycle
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.createdAt
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.notifyCodeConfigurationArn
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.notifyCodeConfigurationId
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.updatedAt
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.createdAt
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.updatedAt
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|2xx->2xx|$.notifyCodeConfiguration.channelParameters.voice
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->2xx|$.message
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->2xx|$.resourceId
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->2xx|$.resourceType
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->2xx|status
Issue-Commit: 87fb877f34cb08a6e89ae1853605cffbee8a1e9e

# endusermessaging notify-code-configuration projections diverge: minted id shape, nanosecond timestamps and stale state reads

> _Note: this issue was identified against the code base at commit `87fb877f34cb08a6e89ae1853605cffbee8a1e9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNotifyCodeConfiguration

## Description
Four net-new root causes in the endusermessaging notify-code-configuration SUCCESS projection and in the state it is read off, all observed replaying the wire corpus simparity-wire-aws-v1 against the running, bootstrapped emulator. 4 causes must be closed before this issue is submitted; they span 6 divergence clusters, every one of which the replay report leaves unowned and unblocked by any stub.

Related but NOT covering issues, named so a fixer can share the work: '20261104T183251Z-endusermessaging-brand-profile-success-bodies-diverge-nanosecond-precision.md' records Cause 2's shared Timestamp helper for the BRAND-PROFILE operations only — its wire keys are CreateBrandProfile/GetBrandProfile/UpdateBrandProfile, so the notify-code-configuration steps below are not gated by it (fixing state.rs::timestamp should close both, but these steps need their own replay). '20261115T011048Z-endusermessaging-updatenotifycodeconfiguration-stub.md' and '20261115T011048Z-endusermessaging-deletenotifycodeconfiguration-stub.md' own those two operations' own 501 answers; Causes 3 and 4 below are the READ-path divergences that outlive them (GetNotifyCodeConfiguration serving state that AWS would have changed or removed), and carry wire keys neither stub issue holds.

=== Cause 1 of 4: the minted notifyCodeConfigurationId does not have the shape AWS's responses carry, and the ARN that embeds it diverges with it ===
AWS: Smithy model com.amazonaws.endusermessaging#EndUserMessaging (restJson1). NotifyCodeConfiguration.notifyCodeConfigurationId is @required, targets NotifyCodeConfigurationId (@length 1..64, @pattern ^[A-Za-z0-9_-]+$, documented 'Bare system-generated NotifyCodeConfiguration identifier (output-only) ... Identifiers are opaque tokens; clients must not assume a specific length or format'), and notifyCodeConfigurationArn is @required, targeting AmazonResourceName (@length 20..256, @pattern ^arn:[A-Za-z0-9_:/-]+$). The recorded responses do not assert the identifier's VALUE (it is minted per run) but do assert its SHAPE, and the emulator's minted identifier fails that assertion; the recorded value's literal digits are not the requirement. Recorded behavior only: AWS mints a system-generated opaque id of a shape this emulator does not reproduce. Hypothesis: the shape assertion is failed by the emulator's short, visibly structured id — a 'ncc-' prefix plus 9 hex digits of clock seed plus an 8-hex ZERO-PADDED monotonic counter (so every id in a run ends ...00000001, ...00000002) — where AWS's opaque token carries a longer, uniformly random body. The ARN diverges because it embeds that id; the region and the emulator's account number in the ARN come from the environment and are NOT part of the requirement.
Emulator: services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:164-189 — mint_notify_code_configuration_id delegates to next_candidate_id('ncc'), which formats '{prefix}-{seed:09x}{counter:08x}', and src/state.rs:299-305 builds notify_code_configuration_arn from it. The replay reports $.notifyCodeConfiguration.notifyCodeConfigurationId (and, on the fixture that asserts it, $.notifyCodeConfiguration.notifyCodeConfigurationArn) as value_mismatch against the recorded shape on every create.
Why: callers and SDK-level fixtures that validate an identifier's shape, size an id column, or rely on ids being unguessable see an identifier AWS never issues; because the body is a zero-padded counter the ids are also sequentially predictable across a run, which anything that exercises id opacity or id-collision handling sees differently than against AWS.
Fix: the service's id minting in state.rs (next_candidate_id / mint_notify_code_configuration_id), with notify_code_configuration_arn following it — mint an opaque identifier of the shape AWS's notify-code-configuration ids carry instead of a prefix plus a clock seed and a zero-padded counter.
Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#1 (id + arn; clustered with endusermessaging-notify-code-configuration-validation-and-idempotency#task#2) and endusermessaging-notify-code-configuration-lifecycle#task#1 (id).

=== Cause 2 of 4: every Timestamp on this surface is serialized as epoch seconds carrying nanosecond resolution, so createdAt/updatedAt do not have the shape AWS's responses carry ===
AWS: NotifyCodeConfiguration.createdAt and .updatedAt are @required, target smithy.api#Timestamp with no @timestampFormat override, so restJson1 carries them in the body as epoch-seconds. The recordings do not assert the clock value (it differs per run) but do assert the member's shape; recorded behavior only: AWS's epoch-seconds timestamps carry at most millisecond fractional precision, never nanosecond resolution. Hypothesis: a 7-fractional-digit double is exactly what the shape assertion rejects.
Emulator: src/state.rs:316-327 — timestamp(SystemTime) is built 'from whole seconds plus sub-second nanoseconds so a stored instant keeps the resolution SystemTime::now() gave it', and src/state.rs:421-434 (notify_code_configuration_view) projects created_at/updated_at through it for EVERY operation that returns a configuration. The replay reports createdAt/updatedAt as value_mismatch against the recorded shape on CreateNotifyCodeConfiguration and GetNotifyCodeConfiguration alike, with values such as 1794710258.5426288 (seven fractional digits) — those digits are this run's clock and are not themselves the requirement.
Why: a client that round-trips, hashes, compares or persists these timestamps, or renders them with a fixed-width/millisecond formatter, sees precision AWS never produces; equality against a value fetched back from the same resource, and any comparison on sub-millisecond digits, behaves differently than against the real service.
Fix: the service's shared Timestamp projection — src/state.rs::timestamp, used by notify_code_configuration_view — must emit epoch-seconds at AWS's resolution rather than carrying the host clock's nanoseconds onto the wire. The same helper is behind the open brand-profile issue named above, so one edit closes both surfaces; these steps still need re-replaying.
Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#1 (create), endusermessaging-notify-code-configuration-lifecycle#task#1 (create, clustered), endusermessaging-notify-code-configuration-validation-and-idempotency#task#3 (create), endusermessaging-notify-code-configuration-lifecycle#task#2 (get, clustered with validation-and-idempotency#task#8), endusermessaging-notify-code-configuration-channels-and-tags#task#6 (get).

=== Cause 3 of 4: a configuration read back after its voice channel parameters were set is missing channelParameters.voice entirely ===
AWS: ChannelParameters models text, voice, notify and whatsApp independently; VoiceParameters carries inlineTemplateBody, languageCode (@length 2..35) and voiceId (@pattern ^[A-Za-z0-9]+$), and UpdateNotifyCodeConfiguration is documented to change only the fields the request supplies. After a request that supplies voice channel parameters is accepted, GetNotifyCodeConfiguration projects them back: channelParameters.voice is present with the supplied inlineTemplateBody, languageCode and voiceId, and the record's updatedAt moves.
Emulator: GetNotifyCodeConfiguration's projection is faithful to the store, but the store was never changed — src/ops/update_notify_code_configuration.rs:170-188 is still the scaffold body and answers Scaffolded::not_implemented, so nothing ever writes channel_parameters after create. The read answers HTTP 200 with channelParameters carrying no voice member at all, and with updatedAt still equal to createdAt (the clustered timestamp divergence above).
Why: a client that configures a voice route and then reads the configuration back to confirm it is told the route does not exist, so its own reconciliation loop either re-applies the change forever or marks the channel unconfigured, while AWS would report it set.
Fix: UpdateNotifyCodeConfiguration's handler and the notify-code-configuration state — merge the supplied channelParameters members into the stored record, bump updated_at, and report the record read back through notify_code_configuration_view. The stub issue named above covers that operation's own 501 answer; this cause is the read-side divergence and must be re-replayed with the fixture step below.
Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#6.

=== Cause 4 of 4: a deleted configuration stays readable — GetNotifyCodeConfiguration answers 200 instead of 404 ResourceNotFoundException ===
AWS: DeleteNotifyCodeConfiguration removes the resource (verifications already in progress are unaffected); a subsequent GetNotifyCodeConfiguration for the deleted identifier is refused with HTTP 404, x-amzn-errortype ResourceNotFoundException, Error.Message 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"', resourceType 'notify-code-configuration' and resourceId the identifier from the request — fixed template text with the resource type and the request's identifier as its only variable parts.
Emulator: the record is never removed, because src/ops/delete_notify_code_configuration.rs is still a scaffold that answers not_implemented, so the configuration remains in state.notify_code_configurations and the read answers HTTP 200 with the full configuration body — the replay reports status expected 404 got 200 with $.message, $.resourceId, $.resourceType and $headers.x-amzn-errortype all missing.
Why: a client that deletes a configuration and then polls until the read 404s never terminates, and any caller treating a successful read as 'the resource exists' keeps using an identifier AWS has removed; teardown/idempotent-delete logic that branches on ResourceNotFoundException takes the wrong branch.
Fix: DeleteNotifyCodeConfiguration's handler and the notify-code-configuration state — remove the record (honouring deletionProtectionEnabled with ConflictException) so that the Get path's existing ResourceNotFoundException branch (src/ops/get_notify_code_configuration.rs:160-172) is the one that answers afterwards.
Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#8, clustered with endusermessaging-notify-code-configuration-lifecycle#task#7 and endusermessaging-notify-code-configuration-validation-and-idempotency#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 13 fixtures replayed, 19 divergence clusters). The 6 clusters below all report stub_path empty and owned_by empty.

Cause 1 — fixture endusermessaging-notify-code-configuration-channels-and-tags step task#1, CreateNotifyCodeConfiguration, 200 vs 200 (affected_count 2, also endusermessaging-notify-code-configuration-validation-and-idempotency#task#2):
  $.notifyCodeConfiguration.notifyCodeConfigurationId: value_mismatch (expected '<shaped-not-asserted>', got 'ncc-8ab2ebfc100000001')
  $.notifyCodeConfiguration.notifyCodeConfigurationArn: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:end-user-messaging:us-west-2:000000000000:notify-code-configuration/ncc-8ab2ebfc100000001')
Same cause, fixture endusermessaging-notify-code-configuration-lifecycle step task#1:
  $.notifyCodeConfiguration.notifyCodeConfigurationId: value_mismatch (expected '<shaped-not-asserted>', got 'ncc-6f9a9cd6500000001')
  The quoted ids/ARNs are this run's minted values and are NOT the requirement; the divergence is the SHAPE of a minted identifier ('ncc-' + 9 hex clock-seed digits + an 8-hex zero-padded counter, 21 characters, visibly sequential). Region and account in the ARN are environment values, not requirements.
  Source: src/state.rs:164-189 (mint_notify_code_configuration_id / next_candidate_id, 'format!("{prefix}-{:09x}{:08x}", self.id_seed, self.next_id & 0xFFFF_FFFF)'), src/state.rs:299-305 (notify_code_configuration_arn).

Cause 2 — same create step and three more clusters, all 200 vs 200:
  channels-and-tags#task#1 / lifecycle#task#1 / validation-and-idempotency#task#3 (CreateNotifyCodeConfiguration): $.notifyCodeConfiguration.createdAt and .updatedAt value_mismatch (expected '<shaped-not-asserted>', got 1794710258.5426288 / 1794710251.269382 / 1794710257.2093947)
  lifecycle#task#2 (GetNotifyCodeConfiguration, clustered with validation-and-idempotency#task#8): both members value_mismatch (got 1794710251.269382)
  channels-and-tags#task#6 (GetNotifyCodeConfiguration): both members value_mismatch (got 1794710258.5426288)
  Source: src/state.rs:316-327 (timestamp(): DateTime::from_secs_and_nanos with subsec_nanos) and src/state.rs:421-434 (notify_code_configuration_view projects created_at/updated_at through it). The epoch values are this run's clock; the divergence is seven fractional digits of epoch seconds on every Timestamp member.

Cause 3 — fixture endusermessaging-notify-code-configuration-channels-and-tags step task#6, GetNotifyCodeConfiguration, 200 vs 200:
  $.notifyCodeConfiguration.channelParameters.voice: missing (expected {'inlineTemplateBody': 'Your code is {{code}}', 'languageCode': 'en-US', 'voiceId': 'Joanna'}, got None)
  updatedAt still equals createdAt (1794710258.5426288), i.e. no update was ever applied to the record.
  Source: src/ops/update_notify_code_configuration.rs:170-188 — handle() binds its arguments and returns crate::scaffolded::Scaffolded::not_implemented("UpdateNotifyCodeConfiguration"), so channel_parameters is only ever written by create (src/ops/create_notify_code_configuration.rs:221-222).

Cause 4 — fixture endusermessaging-notify-code-configuration-channels-and-tags step task#8, GetNotifyCodeConfiguration (affected_count 3: also endusermessaging-notify-code-configuration-lifecycle#task#7 and endusermessaging-notify-code-configuration-validation-and-idempotency#task#11):
  status: expected 404, got 200
  $.message: missing (expected 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"', got None)
  $.resourceId: missing (expected the deleted configuration's id, got None)
  $.resourceType: missing (expected 'notify-code-configuration', got None)
  $headers.x-amzn-errortype: missing (expected ResourceNotFoundException, got None)
  The id inside the expected message is the configuration minted by that run and only fills the template; the fixed text is 'Resource not found - ResourceType="' ... '" ResourceId="' ... '"'.
  Source: src/ops/delete_notify_code_configuration.rs (scaffold, answers not_implemented) leaves the record in state.notify_code_configurations (src/state.rs:125), so the Get path's ResourceNotFoundException branch (src/ops/get_notify_code_configuration.rs:160-172) is never reached.

## Steps to Reproduce
Cause 1:
1. POST /v1/notify-code-configurations {"notifyCodeConfigurationName": "<valid name>"} twice.
2. Read notifyCodeConfigurationId/notifyCodeConfigurationArn from each 200 body: the emulator mints 'ncc-' + 9 hex clock digits + an 8-hex zero-padded counter (…00000001, …00000002), i.e. a 21-character, sequentially predictable identifier, and embeds it in the ARN. Compare the identifier's shape against the recorded responses' opaque system-generated id.

Cause 2:
1. POST /v1/notify-code-configurations with any valid name, then GET /v1/notify-code-configurations/<id>.
2. Read createdAt/updatedAt from both bodies: the emulator returns epoch seconds with seven fractional digits (nanosecond resolution) on every operation that projects a configuration.

Cause 3:
1. Create a configuration with channelParameters {"text": {"inlineTemplateBody": "Your code is {{code}}"}}.
2. PUT /v1/notify-code-configurations/<id> {"channelParameters": {"voice": {"inlineTemplateBody": "Your code is {{code}}", "languageCode": "en-US", "voiceId": "Joanna"}}}.
3. GET /v1/notify-code-configurations/<id>: the emulator's body has no channelParameters.voice member and updatedAt still equals createdAt. AWS reports the voice parameters as supplied, with updatedAt moved.

Cause 4:
1. Create a configuration (deletionProtectionEnabled false) and note its id.
2. DELETE /v1/notify-code-configurations/<id>.
3. GET /v1/notify-code-configurations/<id>: the emulator answers HTTP 200 with the full configuration. AWS answers HTTP 404, x-amzn-errortype ResourceNotFoundException, message 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"', resourceType 'notify-code-configuration', resourceId <id>.

Replay: replay the wire corpus simparity-wire-aws-v1; fixtures endusermessaging-notify-code-configuration-channels-and-tags (task#1, task#6, task#8), endusermessaging-notify-code-configuration-lifecycle (task#1, task#2) and endusermessaging-notify-code-configuration-validation-and-idempotency (task#3).
