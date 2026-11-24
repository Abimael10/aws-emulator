Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: endusermessaging.seeded-attribute-can-be-deleted-and-recreated-with-a-value
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging.seeded-attribute-can-be-deleted-and-recreated-with-a-value|run#6|AssertionError|assert "City" in seeded, f"expected the seeded template to hold City, got {sorted(seeded)!r}"
Issue-Commit: 1b54d60a21bdcb9d9cbbbe8d2d80ba69001d0d46

# endusermessaging new brand profiles are seeded with a 4-element placeholder attribute template instead of the service default template (no "City", no category)

> _Note: this issue was identified against the code base at commit `1b54d60a21bdcb9d9cbbbe8d2d80ba69001d0d46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBrandProfile

## Description
1 net-new cause, grounded in behavioral check endusermessaging.seeded-attribute-can-be-deleted-and-recreated-with-a-value. The open issue 20261115T123417Z-endusermessaging-listbrandprofileattributes-diverges-default-attribute-projection-omits-category.md describes this same seed from the WIRE corpus (Cause 1, fixture endusermessaging-brand-profile-lifecycle#task#4) and the open issue 20261104T183251Z-endusermessaging-brand-profile-success-bodies-diverge-nanosecond-precision.md describes the attributesCreated count side of it; neither carries this behavioral check's replay key, and the replay report does not mark this check as owned or already filed, so it is filed here and named against those issues rather than described as unrelated.

Cause 1 of 1: CreateBrandProfile seeds a placeholder 4-attribute template, so a newly created brand profile does not carry the service's default attribute set and a default attribute cannot be looked up by its real AWS name.
AWS: Smithy (com.amazonaws.endusermessaging#EndUserMessaging, restJson1): CreateBrandProfile output member attributesCreated (Integer, @required) is documented "The number of default attributes that were created for the brand profile." ListBrandProfileAttributes -> brandProfileAttributes (@required, list of BrandProfileAttributeSummary, @length 0..100), GET /v1/brand-profiles/{brandProfileId}/attributes; BrandProfileAttributeSummary.attributeName is @required BrandProfileAttributeName (@length 1..256, pattern ^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$ — so names MAY contain spaces), attributeType is @required enum TEXT|IMAGE|DOCUMENT, and `category` targets BrandProfileAttributeCategory (@length 1..64, "The category of the attribute.", documented as validated against the service's supported values). The model does not enumerate the template itself, so for its contents: recorded behavior only — creating a brand profile seeds the service's full default attribute template, and the subsequent list pages that whole template back, with multi-word human-readable names ("City", "Company Name", "Contact Email", "Address Line 2", "Business Type") each carrying a `category` (e.g. BUSINESS_ADDRESS, BUSINESS_IDENTITY, BUSINESS_CONTACT). The requirement is that the seeded set IS the service's default template — its member names and their categories — not any one recording's element count.
Emulator: ListBrandProfileAttributes on a freshly created profile returns exactly four summaries with single-token placeholder names and no `category` member at all: ['BrandLogo' IMAGE, 'CompanyAddress' TEXT, 'CompanyName' TEXT, 'ComplianceDocument' DOCUMENT]. The names are invented (none of them is an AWS default-template name), the AWS names are absent, and the check fails at its very first assertion because "City" is not in the set.
Why: a client that reads, updates or deletes a default attribute by its AWS name — which is the only way the API addresses an attribute, since GetBrandProfileAttribute/UpdateBrandProfileAttribute/DeleteBrandProfileAttribute key on attributeName — gets ResourceNotFoundException against the emulator for every real default attribute, while the same code succeeds on AWS. Conversely, code enumerating the profile sees four names that do not exist on AWS. Any grouping or filtering by `category` also sees the member unset on every element and cannot classify attributes. Pagination shape differs too, since a 4-element page is not the template's page shape.
Fix: the brand-profile default-attribute seed in the service's state (crate::state::default_brand_profile_attributes, installed by CreateBrandProfile) must install the service's real default attribute template — the AWS attributeNames with their types and categories — and the BrandProfileAttributeSummary serializer used by ListBrandProfileAttributes must emit `category` for every seeded attribute. attributesCreated must then report the number actually seeded. File/line citations: services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/create_brand_profile.rs:219-223 and :240-241 (seed + count), and the state helper it calls.
Traces: behavioral check endusermessaging.seeded-attribute-can-be-deleted-and-recreated-with-a-value.

## Evidence
Check endusermessaging.seeded-attribute-can-be-deleted-and-recreated-with-a-value [mismatch]:
  1. CreateBrandProfile(brandProfileName=<unique>) -> 200.
  2. Paginate ListBrandProfileAttributes(brandProfileId=<new profile>) and collect attributeName.
  AssertionError: expected the seeded template to hold City, got ['BrandLogo', 'CompanyAddress', 'CompanyName', 'ComplianceDocument']

  expected: the collected attributeName set is the service's default attribute template and contains the AWS default-template names, including 'City' (TEXT, category BUSINESS_ADDRESS), alongside e.g. 'Company Name', 'Contact Email', 'Address Line 2'; every element carries `category`.
  actual:   exactly 4 elements — BrandLogo (IMAGE), CompanyAddress (TEXT), CompanyName (TEXT), ComplianceDocument (DOCUMENT) — none of which is an AWS default-template name, and none carrying `category`.

The check never reaches its later assertions because of this; those later assertions (DeleteBrandProfileAttribute on a seeded name, then GetBrandProfileAttribute refused with ResourceNotFoundException message 'Resource not found - ResourceType="brand-profile-attribute"', then CreateBrandProfileAttributes re-creating the same name with attributeType TEXT / attributeValue supplied by the caller) are blocked behind the seed and behind the already-filed CreateBrandProfileAttributes / DeleteBrandProfileAttribute stubs. Element names and types above are the emulator's fixed placeholders, not per-run values; the profile id is a per-run value and is not part of the requirement.

## Steps to Reproduce
1. POST /v1/brand-profiles with {"brandProfileName": "<any name matching ^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$"} (CreateBrandProfile) and note brandProfileId.
2. GET /v1/brand-profiles/{brandProfileId}/attributes (ListBrandProfileAttributes), paging to exhaustion.
3. Observe the returned brandProfileAttributes: the emulator returns 4 summaries named BrandLogo, CompanyAddress, CompanyName, ComplianceDocument with no `category`. AWS returns the service's default attribute template, which includes 'City' (TEXT, category BUSINESS_ADDRESS) and the other multi-word default names, each with a `category`.
4. GetBrandProfileAttribute(brandProfileId, attributeName="City") — succeeds on AWS, refused with ResourceNotFoundException on the emulator because the name was never seeded.
Replay: behavioral check endusermessaging.seeded-attribute-can-be-deleted-and-recreated-with-a-value.
