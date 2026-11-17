Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-deletion-protection
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-deletion-protection endusermessaging-brand-profile-idempotent-create endusermessaging-brand-profile-lifecycle endusermessaging-brand-profile-tagging
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|CreateBrandProfile|2xx->2xx|$.attributesCreated
Issue-Commit: d56f3e72fadd2bcc773c18998c38d59f68ab7320

# endusermessaging CreateBrandProfile attributesCreated undercounts the service default attribute template

> _Note: this issue was identified against the code base at commit `d56f3e72fadd2bcc773c18998c38d59f68ab7320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBrandProfile

## Description
1 net-new cause. The open issues 20261116T003409Z-endusermessaging-new-brand-profiles-are-seeded-with-a.md (behavioral, the seeded attribute NAMES and categories) and 20261115T123417Z-endusermessaging-listbrandprofileattributes-diverges-default-attribute-projection-omits-category.md (the list projection of that seed) describe the same underlying seed; neither carries this wire step's replay key and the replay report marks this cluster neither owned nor stub-blocked, so the attributesCreated side of it is filed here and named against those issues rather than described as unrelated.

Cause 1 of 1: CreateBrandProfile reports attributesCreated from a 4-slot placeholder template, so every successful create understates how many default attributes the new profile actually holds on AWS.
AWS: Smithy (com.amazonaws.endusermessaging#EndUserMessaging, restJson1): CreateBrandProfile POST /v1/brand-profiles -> 200; output member attributesCreated targets Integer and is @required, documented "The number of default attributes that were created for the brand profile." The model does not enumerate the template itself, so for its size: recorded behavior only - every recorded CreateBrandProfile response reports 25, the size of the service's default attribute template, which is the same template ListBrandProfileAttributes pages back (multi-word default names such as 'Address Line 2', 'Business Type', 'City', 'Company Address', 'Company Name', 'Contact Email', each carrying a category). The requirement is that attributesCreated equal the number of default attributes the create seeded, and that the seed BE the service's default template - not any one recording's literal integer.
Emulator: 200 with attributesCreated = 4 on every CreateBrandProfile, in every fixture that creates a profile. The count is faithful to the emulator's own seed - src/state/brand_profile_attributes.rs:188-205 defines DEFAULT_BRAND_PROFILE_ATTRIBUTES as exactly four invented placeholder slots (CompanyName/TEXT, CompanyAddress/TEXT, ComplianceDocument/DOCUMENT, BrandLogo/IMAGE), and BrandProfile.default_attributes_created (src/state.rs:60-64) records that length - so the count is wrong because the seed is wrong, not because the count is computed wrongly.
Why: attributesCreated is a @required member of the create response, so it is the first thing a client learns about the new profile. A client that uses it to size its own view of the profile, to decide whether the default template was provisioned, to drive a progress or quota calculation against the 0..100 ceiling of brandProfileAttributes, or simply to assert the create succeeded as documented, reads 4 where AWS reports the full template, and a subsequent ListBrandProfileAttributes then pages back four names that do not exist on AWS - so the whole default-attribute surface is unusable even though the create answered 200.
Fix: the brand-profile default-attribute seed in this service's state (DEFAULT_BRAND_PROFILE_ATTRIBUTES / default_brand_profile_attributes in src/state/brand_profile_attributes.rs, installed by the CreateBrandProfile handler in src/ops/create_brand_profile.rs) must install the service's real default attribute template - the AWS default attributeNames with their attributeTypes and categories - and attributesCreated must report the number actually seeded, so the count and the list agree by construction rather than by a hardcoded number.
Traces: endusermessaging-brand-profile-deletion-protection#task#1, and the same cluster's endusermessaging-brand-profile-idempotent-create#task#1 / #task#3, endusermessaging-brand-profile-lifecycle#task#1, endusermessaging-brand-profile-tagging#task#1.

## Evidence
Replayed against the emulator from this checkout. Cluster endusermessaging-brand-profile-deletion-protection#task#1, CreateBrandProfile, expected 200 / actual 200 (5 affected steps across 5 fixtures):
  $.attributesCreated: value_mismatch (expected 25, got 4)

  expected: attributesCreated equals the size of the service's default attribute template seeded into the new profile (25 in every recording), and ListBrandProfileAttributes then pages exactly that template back.
  actual:   attributesCreated = 4 on every create, matching the emulator's four invented placeholder slots (CompanyName, CompanyAddress, ComplianceDocument, BrandLogo).

The integer 25 is the recorded template size, not a per-run value: all five affected steps, across five independent fixtures, expect the same count, and the emulator returns 4 for all five. The brandProfileId/brandProfileArn and the timestamps in those same responses are per-run values and are not part of this requirement.

## Steps to Reproduce
1. POST /v1/brand-profiles with {"brandProfileName": "<any name matching ^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$"} (CreateBrandProfile).
2. Read attributesCreated from the 200 response: the emulator reports 4; AWS reports the number of default attributes in the service's default attribute template (25 in every recording).
3. GET /v1/brand-profiles/{brandProfileId}/attributes (ListBrandProfileAttributes), paging to exhaustion, and count the returned brandProfileAttributes: the count must equal attributesCreated and the names must be the service's default template names.
Replay: fixture endusermessaging-brand-profile-deletion-protection step task#1 (same cluster: endusermessaging-brand-profile-idempotent-create#task#1 and #task#3, endusermessaging-brand-profile-lifecycle#task#1, endusermessaging-brand-profile-tagging#task#1).
