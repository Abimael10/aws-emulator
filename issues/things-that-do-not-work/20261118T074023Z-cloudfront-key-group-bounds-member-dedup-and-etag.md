Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-key-group-config-bounds-and-member-dedup
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-key-group-config-bounds-and-member-dedup cloudfront-key-group-listing-parameter-rejections
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreateKeyGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|CreateKeyGroup|4xx->2xx|$.ErrorResponse
Issue-Key: cloudfront|CreateKeyGroup|4xx->2xx|status
Issue-Key: cloudfront|CreateKeyGroup|2xx->2xx|$.KeyGroup.KeyGroupConfig.Items.PublicKey
Issue-Key: cloudfront|GetKeyGroupConfig|2xx->2xx|$.KeyGroupConfig.Items.PublicKey
Issue-Key: cloudfront|UpdateKeyGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|UpdateKeyGroup|4xx->2xx|$.ErrorResponse
Issue-Key: cloudfront|UpdateKeyGroup|4xx->2xx|status
Issue-Key: cloudfront|UpdateKeyGroup|2xx->4xx|$.KeyGroup
Issue-Key: cloudfront|UpdateKeyGroup|2xx->4xx|$headers.etag
Issue-Key: cloudfront|UpdateKeyGroup|2xx->4xx|status
Issue-Key: cloudfront|GetKeyGroupConfig|2xx->2xx|$.KeyGroupConfig.Comment
Issue-Key: cloudfront|GetKeyGroupConfig|2xx->2xx|$headers.etag
Issue-Key: cloudfront|ListKeyGroups|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 9fdcce976c9d3a333330e6c021629b669a23ff81

# CloudFront key group bounds, member dedup and ETag handling diverge from AWS

> _Note: this issue was identified against the code base at commit `9fdcce976c9d3a333330e6c021629b669a23ff81`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateKeyGroup

## Description
NINE (9) distinct net-new causes in the CloudFront key-group surface. All nine must be closed before this issue is submitted. Replay reports each of them unowned by any open issue; the nearest open issues (20261117T151821Z-cloudfront-5-net-new-false-refusal-causes-key.md, 20261117T151821Z-cloudfront-6-net-new-refusal-wording-causes-emulator.md and 20261108T183800Z-cloudfront-createkeygroup-accepts-a-keygroupconfig-items-entry-naming.md) cover key-group quota counting, other refusal wordings and a non-existent public key id, NOT the causes below.

=== Cause 1 of 9: CreateKeyGroup's refusal of an empty KeyGroupConfig.Items uses emulator-authored prose ===
AWS: Smithy: CreateKeyGroup input KeyGroupConfig @required, httpPayload; KeyGroupConfig.Items targets PublicKeyIdList and is @required; modeled error InvalidArgument (errorKind client, httpError 400, doc "An argument is invalid."). The recorded AWS response is HTTP 400 with ErrorResponse.Error.Type 'Sender', Code 'InvalidArgument', Message 'The parameter publicKeyIdsList is invalid.' The whole sentence is fixed text apart from the parameter name, which AWS reports as the wire-level parameter name of the offending member (publicKeyIdsList for KeyGroupConfig.Items) taken from the request.
Emulator: HTTP 400 / InvalidArgument but Message 'An argument is invalid: to create a key group, you must specify at least one public key for the key group' — emulator-authored prose that names no parameter.
Why: a client or support tool that branches on the refusal text (or surfaces it to an operator) cannot recognize the AWS refusal, and the parameter name a caller needs to fix is absent.
Fix: CreateKeyGroup's input validation in the cloudfront key-group handler — the message template for an invalid publicKeyIdsList.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#2 (representative), #task#5.

=== Cause 2 of 9: CreateKeyGroup accepts an over-long KeyGroupConfig.Name ===
AWS: Smithy: KeyGroupConfig.Name @required string; error InvalidArgument (client, 400). Recorded AWS response for a Name longer than the service's key-group-name bound: HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'The parameter KeyGroupName is too big.' The entire message is fixed text; only the modeled parameter label KeyGroupName appears, so no request value is interpolated.
Emulator: HTTP 201 with no ErrorResponse at all — the key group is created with the out-of-bounds name.
Why: a caller that exceeds the name bound gets a resource AWS would never have created, so subsequent signed-URL/key-group wiring built on the emulator passes where real AWS fails at creation time, and the client never learns which parameter was wrong.
Fix: CreateKeyGroup's input validation — add the KeyGroupName length bound check before state mutation.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#3 (representative), #task#4.

=== Cause 3 of 9: CreateKeyGroup stores repeated public key ids instead of collapsing them ===
AWS: recorded behavior only — the Smithy model places no @uniqueItems on PublicKeyIdList, so the de-duplication is not modeled. The recording shows that when KeyGroupConfig.Items names the same public key id twice, the 201 CreateKeyGroup response's KeyGroup.KeyGroupConfig.Items carries that id exactly ONCE (a single PublicKey element), i.e. AWS collapses repeated members of the id list.
Emulator: HTTP 201 but KeyGroup.KeyGroupConfig.Items.PublicKey is a two-element list repeating the same id (type_mismatch: a list where AWS serializes one scalar element).
Why: the shape of the readback changes — a client deserializing Items gets two entries where AWS gives one, so member counts, idempotent re-submits and any quota arithmetic the client does over Items are all off, and XML-shape-sensitive parsers see a repeated element where a single one is expected.
Fix: CreateKeyGroup's state write for the key group's public-key id list (de-duplicate on store), with the serializer following the stored list.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#6.

=== Cause 4 of 9: GetKeyGroupConfig reads back the same duplicated public key ids ===
AWS: recorded behavior only, as in cause 3. Smithy: GetKeyGroupConfig GET /2020-05-31/key-group/{Id}/config, 200, output KeyGroupConfig httpPayload + ETag header. The recording shows KeyGroupConfig.Items carrying the id once.
Emulator: HTTP 200 but KeyGroupConfig.Items.PublicKey is a two-element list repeating the same id.
Why: the duplication is not confined to the create response — it is persisted and observable on every later read, so a read-modify-write round trip re-submits the duplicate and the divergence compounds.
Fix: the key-group readback path (GetKeyGroupConfig serializer reading the stored id list); it is the same stored-state defect as cause 3 but is a separate observable response and must be verified separately.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#7.

=== Cause 5 of 9: UpdateKeyGroup's refusal of an empty KeyGroupConfig.Items uses emulator-authored prose, and names the wrong operation ===
AWS: Smithy: UpdateKeyGroup PUT /2020-05-31/key-group/{Id}, KeyGroupConfig @required, Items @required; modeled error InvalidArgument (client, 400). Recorded AWS response: HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'The parameter publicKeyIdsList is invalid.' — fixed text with the wire parameter name of the offending member.
Emulator: HTTP 400 / InvalidArgument with Message 'An argument is invalid: to create a key group, you must specify at least one public key for the key group' — not AWS's wording, and it describes creation on an update request.
Why: same text-matching break as cause 1, compounded by the message naming an operation the caller did not invoke, which misdirects anyone debugging from the error alone.
Fix: UpdateKeyGroup's input validation message template (share the publicKeyIdsList template with CreateKeyGroup rather than re-wording it).
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#8 (representative), #task#9.

=== Cause 6 of 9: UpdateKeyGroup accepts a KeyGroupConfig referencing a public key that is not in the key group ===
AWS: Smithy: UpdateKeyGroup errors include InvalidArgument (client, 400). Recorded AWS response: HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'The specified CloudFront public key does not exist in key group.' The whole message is fixed text — no request value is interpolated.
Emulator: HTTP 200 with no ErrorResponse — the update is applied.
Why: the emulator silently accepts a key-group membership edit AWS refuses, so the key group ends up in a state real CloudFront cannot reach; any client that depends on that refusal to detect a stale or wrong public key id never sees it, and signed-URL trust configuration drifts undetected.
Fix: UpdateKeyGroup's validation — check every id in the submitted Items against the key group's existing membership/the account's public keys before applying the update.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#10.

=== Cause 7 of 9: UpdateKeyGroup answers 412 PreconditionFailed to a request whose If-Match is current ===
AWS: Smithy: UpdateKeyGroup input IfMatch (httpHeader If-Match), success code 200 with KeyGroup httpPayload and an ETag response header; PreconditionFailed (client, 412, doc "The precondition in one or more of the request fields evaluated to false.") is reserved for an If-Match that does NOT match the resource's current ETag. The recording shows HTTP 200 with the updated KeyGroup (Id, LastModifiedTime, KeyGroupConfig with Name, Items and Comment) and a fresh ETag header.
Emulator: HTTP 412 PreconditionFailed, with no KeyGroup body and no ETag header, for an If-Match the recording establishes as current.
Why: a correct read-modify-write loop (GET config, edit, PUT with the ETag just read) cannot make progress — the client is told its precondition failed when it did not, so it retries forever or reports a spurious concurrent-modification conflict. Note this step follows cause 6's wrongly-accepted update, so the emulator's ETag ledger has drifted; the defect is that the emulator's recorded ETag for the key group does not track the state a client can observe.
Fix: UpdateKeyGroup's If-Match/ETag evaluation and the key group's ETag state (the ETag must be derived from the state the readback path publishes).
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#11.

=== Cause 8 of 9: GetKeyGroupConfig after an update drops Comment and publishes a non-current ETag ===
AWS: Smithy: KeyGroupConfig.Comment is an optional string (doc: "A comment to describe the key group. The comment cannot be longer than 128 characters."); GetKeyGroupConfig returns KeyGroupConfig as httpPayload plus an ETag header. The recording shows the Comment that was submitted on the preceding update echoed verbatim, Items carrying each public key id once, and the ETag equal to the one the update returned.
Emulator: HTTP 200 but KeyGroupConfig.Comment is MISSING entirely, Items.PublicKey is a two-element list (the submitted id plus a second id), and the ETag header does not equal the ETag the preceding update published.
Why: a submitted optional member vanishing from the readback means a round trip loses caller data; a readback ETag that does not match the one the mutation returned makes every subsequent conditional request unreliable (this is what cause 7 then trips over).
Fix: the key-group state write and the GetKeyGroupConfig serializer — persist and emit optional members (Comment) that were submitted, and publish the ETag that corresponds to the served state.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#12.

=== Cause 9 of 9: ListKeyGroups' invalid-Marker refusal uses emulator-authored prose ===
AWS: Smithy: ListKeyGroups GET /2020-05-31/key-group with Marker (httpQuery Marker) and MaxItems (httpQuery MaxItems); modeled error InvalidArgument (client, 400). Recorded AWS response for a Marker that is not a valid key-group pagination marker: HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'The request contains an invalid key group marker.' The entire message is fixed text — AWS does NOT echo the submitted marker value.
Emulator: HTTP 400 / InvalidArgument with Message 'Marker must be a valid CF key group Id' — emulator-authored wording, and it also mis-states the rule (AWS describes the value as a pagination marker, not as a key group Id).
Why: callers that detect an exhausted or corrupted pagination cursor by the refusal text do not recognize it, and the message tells an operator to supply a key group Id where AWS expects an opaque marker.
Fix: ListKeyGroups' Marker validation message in the key-group pagination path.
Traces: cloudfront-key-group-listing-parameter-rejections#task#1.

## Evidence
Cause 1 — CreateKeyGroup, empty Items (cloudfront-key-group-config-bounds-and-member-dedup#task#2, #task#5): expected 400 Message 'The parameter publicKeyIdsList is invalid.'; actual 400 Message 'An argument is invalid: to create a key group, you must specify at least one public key for the key group'.
Cause 2 — CreateKeyGroup, over-long Name (#task#3, #task#4): expected status 400 with ErrorResponse {Error:{Type:'Sender', Code:'InvalidArgument', Message:'The parameter KeyGroupName is too big.'}}; actual status 201, ErrorResponse missing.
Cause 3 — CreateKeyGroup, duplicate ids (#task#6): expected $.KeyGroup.KeyGroupConfig.Items.PublicKey a single scalar id; actual a 2-element list repeating the same id (type_mismatch).
Cause 4 — GetKeyGroupConfig (#task#7): expected $.KeyGroupConfig.Items.PublicKey a single scalar id; actual a 2-element list repeating the same id (type_mismatch).
Cause 5 — UpdateKeyGroup, empty Items (#task#8, #task#9): expected 400 Message 'The parameter publicKeyIdsList is invalid.'; actual 400 Message 'An argument is invalid: to create a key group, you must specify at least one public key for the key group'.
Cause 6 — UpdateKeyGroup, key not in group (#task#10): expected status 400 with ErrorResponse {Error:{Type:'Sender', Code:'InvalidArgument', Message:'The specified CloudFront public key does not exist in key group.'}}; actual status 200, ErrorResponse missing.
Cause 7 — UpdateKeyGroup with current If-Match (#task#11): expected status 200 with $.KeyGroup (Id, LastModifiedTime, KeyGroupConfig{Name, Items, Comment}) and an $headers.etag; actual status 412 PreconditionFailed, $.KeyGroup missing, $headers.etag missing.
Cause 8 — GetKeyGroupConfig after update (#task#12): expected $.KeyGroupConfig.Comment present (the comment submitted on the update), $.KeyGroupConfig.Items.PublicKey a single scalar id, $headers.etag equal to the update's ETag; actual Comment missing, Items.PublicKey a 2-element list, $headers.etag a different value.
Cause 9 — ListKeyGroups, bad Marker (cloudfront-key-group-listing-parameter-rejections#task#1): expected 400 Message 'The request contains an invalid key group marker.'; actual 400 Message 'Marker must be a valid CF key group Id'.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture cloudfront-key-group-config-bounds-and-member-dedup and compare steps task#2 through task#12 against the recording:
   - task#2 / task#5: CreateKeyGroup with KeyGroupConfig.Items empty -> compare the InvalidArgument message.
   - task#3 / task#4: CreateKeyGroup with a KeyGroupConfig.Name longer than the service bound -> expect 400, observe 201.
   - task#6: CreateKeyGroup with the same public key id listed twice in Items -> compare Items in the 201 body.
   - task#7: GetKeyGroupConfig on that key group -> compare Items.
   - task#8 / task#9: UpdateKeyGroup with KeyGroupConfig.Items empty -> compare the InvalidArgument message.
   - task#10: UpdateKeyGroup with a KeyGroupConfig naming a public key not in the key group -> expect 400, observe 200.
   - task#11: UpdateKeyGroup with the If-Match the previous step published -> expect 200 + KeyGroup + ETag, observe 412.
   - task#12: GetKeyGroupConfig -> compare Comment, Items and the ETag header against the update's ETag.
3. Replay fixture cloudfront-key-group-listing-parameter-rejections step task#1: ListKeyGroups with an invalid Marker -> compare the InvalidArgument message.
