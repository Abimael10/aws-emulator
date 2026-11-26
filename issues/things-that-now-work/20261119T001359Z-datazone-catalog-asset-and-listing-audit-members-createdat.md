Issue-Source: trace-sweep-divergence
Issue-Trace: datazone-custom-asset-publish-to-catalog-listing
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: datazone-custom-asset-publish-to-catalog-listing
Issue-Shape-Id: com.amazonaws.datazone#DataZone
Issue-Key: datazone|CreateAsset|2xx->2xx|$.createdAt
Issue-Key: datazone|CreateAsset|2xx->2xx|$.createdBy
Issue-Key: datazone|CreateAsset|2xx->2xx|$.firstRevisionCreatedAt
Issue-Key: datazone|CreateAsset|2xx->2xx|$.firstRevisionCreatedBy
Issue-Key: datazone|GetListing|2xx->2xx|$.createdAt
Issue-Key: datazone|GetListing|2xx->2xx|$.createdBy
Issue-Key: datazone|GetListing|2xx->2xx|$.item.assetListing.createdAt
Issue-Key: datazone|GetListing|2xx->2xx|$.updatedAt
Issue-Key: datazone|GetListing|2xx->2xx|$.updatedBy
Issue-Commit: cad5ca51a10daa37e6769d2d4605af91fcdd454e

# datazone catalog asset and listing audit members (createdAt/createdBy/updatedAt) fail the recorded wire shape (2 causes)

> _Note: this issue was identified against the code base at commit `cad5ca51a10daa37e6769d2d4605af91fcdd454e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAsset

## Description
TWO net-new causes, both in how the datazone crate RENDERS the audit members of a successful catalog read/write. Cause count: 2. Every affected step answered the right HTTP status (201 for CreateAsset, 200 for GetListing) and every other member of the body matched the recording; only the attribution members — the `createdAt`/`updatedAt`/`firstRevisionCreatedAt` timestamps and the `createdBy`/`updatedBy`/`firstRevisionCreatedBy` principals — are rejected, and they are rejected on EVERY audit member of both responses. Both causes come out of the same two shared helpers in this crate (`crate::state::date_time` and `crate::state::caller_principal`), so they are reported together; fix both before submitting.

The corpus asserts the SHAPE of these members, not their instants or principals (the replay reports `expected '<shaped-not-asserted>'`), because the instant of a run and the identity of the signing key are per-run values and are not requirements. What diverges is therefore the ENCODING/form of the value, not the value.

=== Cause 1 of 2: CreateAsset reports all four asset audit members in a shape the recorded response does not use ===
AWS: Smithy (com.amazonaws.datazone#DataZone, restJson1, POST /v2/domains/{domainIdentifier}/assets, success 201): `CreateAssetOutput$createdAt` and `$firstRevisionCreatedAt` target `com.amazonaws.datazone#CreatedAt`, a plain `timestamp` with NO `@timestampFormat` override; `$createdBy` and `$firstRevisionCreatedBy` target `com.amazonaws.datazone#CreatedBy`, a plain `string` documented as "The Amazon DataZone user that created this asset in the catalog" / "The Amazon DataZone user that made the first revision of the asset". The model states nothing further about either encoding, so for the exact wire form this is RECORDED BEHAVIOR ONLY: in the recording all four members are present on the 201 body and carry values whose shape the corpus pins, and the emulator's values do not satisfy that shape. The requirement is the shape of the encoding — a DataZone-attributable user identifier for the `CreatedBy` members and the catalog's own timestamp encoding for the `CreatedAt` members — never the recording's particular instant or principal.
Emulator: 201 with all four members present but shaped from host-local sources: `createdAt` = `firstRevisionCreatedAt` = a fractional epoch-seconds JSON number taken straight from the host wall clock (this run: 1795046870.352), and `createdBy` = `firstRevisionCreatedBy` = a SYNTHESIZED IAM-user ARN built out of the caller's access key id (this run: arn:aws:iam::000000000000:user/EMULATORACC2A0603403). Both come from shared helpers: `crate::state::date_time(ms)` is `DateTime::from_secs_f64(ms as f64 / 1000.0)`, i.e. the stored epoch-millisecond instant with its sub-second remainder kept, and `crate::state::caller_principal(ctx)` is `engine::emulator_arn("iam", "", &format!("user/{akid}"))`, i.e. the access key id pasted in where an IAM user NAME belongs. `asset_output` in ops/create_asset.rs fills all four members from those two helpers. Hypothesis: the sub-second tail on the timestamps and the `iam ... user/<access-key-id>` principal form are exactly what the shaped assertion rejects — the four members fail uniformly, in one cluster, while every non-audit member of the same body (id, name, revision, typeIdentifier/typeRevision, formsOutput, owningProjectId, domainId) passes, so the failure is in how these members are rendered and not in the asset the handler stored.
Why: a client that reads the catalog's audit trail gets values of a different kind than AWS serves. `createdBy` is the handle a caller uses to look the creator up as a DataZone user (GetUserProfile / project membership), and an access-key id dressed up as an IAM user name names no principal that exists, so attribution and any "who created this asset" join break. The timestamps come back at a resolution AWS does not emit, so a response digest, a recorded-fixture comparison or a strict date parser disagrees with the emulator on every asset create even though the underlying instant is fine.
Fix: the two shared rendering helpers in the datazone crate's state module — `crate::state::date_time` (truncate the stored instant to the resolution AWS's catalog responses use before building the wire `DateTime`) and `crate::state::caller_principal` (derive a DataZone-attributable user identifier for the caller instead of pasting the access key id into an IAM user ARN) — plus their use in `asset_output`. No per-member change is needed: every operation that reports an audit member routes through these two.
Traces: datazone-custom-asset-publish-to-catalog-listing#task#7.

=== Cause 2 of 2: GetListing reports every listing audit member, and the nested asset-listing createdAt, in the same wrong shape ===
AWS: Smithy (restJson1, GET /v2/domains/{domainIdentifier}/listings/{identifier}, success 200): `GetListingOutput$createdAt` -> `CreatedAt` (plain timestamp), `$updatedAt` -> `UpdatedAt` (plain timestamp), `$createdBy` -> `CreatedBy` (string), `$updatedBy` -> `UpdatedBy` (string), and `$item` -> `ListingItem`, whose `assetListing` member carries its own `createdAt` -> `CreatedAt`, documented as "The timestamp of when an asset published in an Amazon DataZone catalog was created". None carries a `@timestampFormat` or any further constraint, so again the exact wire form is RECORDED BEHAVIOR ONLY: the recording carries all five members and the corpus pins their shape, which the emulator's values do not satisfy.
Emulator: 200 with all five members present and all five rejected — `$.createdAt` and `$.updatedAt` fractional epoch-seconds numbers from the host clock (this run 1795046870.368 and 1795046870.668), `$.item.assetListing.createdAt` likewise (1795046870.352, the published asset's own stored instant), and `$.createdBy`/`$.updatedBy` the same synthesized access-key-derived IAM user ARN (arn:aws:iam::000000000000:user/EMULATORACC2A0603403). Source: `ops/get_listing.rs` fills `created_at`/`updated_at` with `date_time(listing.created_at_ms)` / `date_time(listing.updated_at_ms)` and `created_by`/`updated_by` with the principal `crate::state::caller_principal` stored on the `Listing` by the publishing change set; `crate::state::listing_item` fills the nested `assetListing.createdAt` with `date_time(revision.entity_created_at_ms)`. Every other member of the listing and of its nested asset listing (domainId, id, listingRevision, name, status, assetId, assetRevision, assetType, forms, glossaryTerms, owningProjectId) matched.
Why: the catalog read is the surface a consumer uses to decide whether a listing is newer than what it holds and who published it. A sub-second-precision timestamp AWS never emits defeats cache/delta comparisons that round-trip the field, and a principal string that is not a DataZone user identifier cannot be resolved back to the publisher — so a consumer cannot attribute a published listing at all.
Fix: same two helpers as cause 1, as consumed by the GetListing handler and by `crate::state::listing_item` (the nested asset-listing projection). Note the two OPEN codegen issues `20261021T223000Z-scaffold-codegen-drops-timestampformat-from-member-schemas.md` and `20261022T000000Z-smithy-macros-drops-timestampformat-from-member-schemas.md` do NOT cover this: this service's model declares no `@timestampFormat` on `CreatedAt`/`UpdatedAt`, so nothing is being dropped — the divergence is in the value this crate builds, not in the schema it is serialized against. The open datazone issue `20261029T172524Z-datazone-error-messages-diverge-from-aws-wording-in.md` covers only error-message wording on 4xx paths and does not cover these success bodies.

Shared root cause: the datazone crate renders every audit member from host-local state (`date_time` over the raw wall clock, `caller_principal` over the request's access key id) rather than in the shapes DataZone's catalog responses use.

## Evidence
Wire corpus simparity-wire-aws-v1, replayed against the emulator built from this checkout (14 fixtures, 125 steps, 51 passed, 74 mismatched of which 66 are unimplemented-operation steps already filed, 8 divergence clusters). Six of the eight clusters are owned by 20261029T172524Z-datazone-error-messages-diverge-from-aws-wording-in.md; the two bundled here are net-new. Verbatim from the replay report:

1) CreateAsset, fixture datazone-custom-asset-publish-to-catalog-listing, step task#7, expected 201 / actual 201:
   $.createdAt: value_mismatch (expected '<shaped-not-asserted>', got 1795046870.352)
   $.createdBy: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:iam::000000000000:user/EMULATORACC2A0603403')
   $.firstRevisionCreatedAt: value_mismatch (expected '<shaped-not-asserted>', got 1795046870.352)
   $.firstRevisionCreatedBy: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:iam::000000000000:user/EMULATORACC2A0603403')
   divergence_key: service=datazone|CreateAsset|status=201->201|keys=$.createdAt,$.createdBy,$.firstRevisionCreatedAt,$.firstRevisionCreatedBy

2) GetListing, same fixture, step task#9, expected 200 / actual 200:
   $.createdAt: value_mismatch (expected '<shaped-not-asserted>', got 1795046870.368)
   $.createdBy: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:iam::000000000000:user/EMULATORACC2A0603403')
   $.item.assetListing.createdAt: value_mismatch (expected '<shaped-not-asserted>', got 1795046870.352)
   $.updatedAt: value_mismatch (expected '<shaped-not-asserted>', got 1795046870.668)
   $.updatedBy: value_mismatch (expected '<shaped-not-asserted>', got 'arn:aws:iam::000000000000:user/EMULATORACC2A0603403')
   divergence_key: service=datazone|GetListing|status=200->200|keys=$.createdAt,$.createdBy,$.item.assetListing.createdAt,$.updatedAt,$.updatedBy

Expected (AWS): each audit timestamp an epoch-seconds value in the shape DataZone's catalog responses use, and each audit principal a DataZone-attributable user identifier for the caller.
Actual (emulator): fractional epoch-seconds numbers straight off the host wall clock, and `arn:aws:iam::<account>:user/<access-key-id>` for every principal.
The literal epoch numbers, the account id 000000000000 and the key id EMULATORACC2A0603403 above are per-run values and are NOT the requirement — only the shape of the encoding is.
Code behind both causes: `date_time` (DateTime::from_secs_f64(ms as f64 / 1000.0)) and `caller_principal` (emulator_arn("iam", "", "user/{akid}")) in services/service-com-amazonaws-datazone-datazone/src/state.rs, consumed by `asset_output` in src/ops/create_asset.rs (createdAt/createdBy/firstRevisionCreatedAt/firstRevisionCreatedBy), by src/ops/get_listing.rs (createdAt/updatedAt/createdBy/updatedBy) and by `listing_item` in src/state.rs (item.assetListing.createdAt).

## Steps to Reproduce
1. Boot the emulator from this checkout and point a datazone client at it.
2. create_domain(...), then create_project(domainIdentifier=D, ...) in that domain, create_form_type / create_asset_type as the fixture does.
3. create_asset(domainIdentifier=D, owningProjectIdentifier=P, typeIdentifier=T, name=N) and read the RAW 201 JSON body (not the SDK-parsed values): createdAt and firstRevisionCreatedAt are fractional epoch-seconds numbers off the host clock, and createdBy/firstRevisionCreatedBy are arn:aws:iam::<account>:user/<access-key-id> — the access key id used as an IAM user name. Cause 1.
4. create_listing_change_set(domainIdentifier=D, entityIdentifier=<assetId>, entityType=ASSET, action=PUBLISH), then get_listing(domainIdentifier=D, identifier=<listingId>) and read the raw 200 body: createdAt, updatedAt and item.assetListing.createdAt carry the same host-clock fractional epoch encoding and createdBy/updatedBy the same synthesized ARN. Cause 2.
5. Re-replay both steps of the wire corpus to confirm the causes are closed: datazone-custom-asset-publish-to-catalog-listing#task#7 (CreateAsset) and datazone-custom-asset-publish-to-catalog-listing#task#9 (GetListing).
