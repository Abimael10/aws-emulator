Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-lifecycle endusermessaging-brand-profile-identifier-and-list-errors
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|ListBrandProfileAttributes|2xx->2xx|$.brandProfileAttributes[*]
Issue-Key: endusermessaging|ListBrandProfileAttributes|4xx->4xx|$.message
Issue-Commit: eedcbd539e91aa3bbb4add86fe8b39999130d759

# endusermessaging ListBrandProfileAttributes diverges: default attribute projection omits category and pagination-token refusal is re-worded

> _Note: this issue was identified against the code base at commit `eedcbd539e91aa3bbb4add86fe8b39999130d759`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListBrandProfileAttributes

## Description
ListBrandProfileAttributes is reached and answers (it is NOT a stub on this branch, so the open issue 20261110T210354Z-endusermessaging-listbrandprofileattributes-stub.md does not describe what it now does and does not carry these replay keys), but both its success projection and its pagination-token refusal diverge from the recording. 2 causes.

Cause 1 of 2: ListBrandProfileAttributes returns the wrong default attribute set and omits the `category` member. Fixture endusermessaging-brand-profile-lifecycle, step task#4.
AWS: Smithy: ListBrandProfileAttributes -> ListBrandProfileAttributesOutput.brandProfileAttributes (@required, list of BrandProfileAttributeSummary, @length 0..100), GET /v1/brand-profiles/{brandProfileId}/attributes. BrandProfileAttributeSummary members: attributeName (@required, BrandProfileAttributeName), attributeType (@required, enum BrandProfileAttributeType), description, category (BrandProfileAttributeCategory, @length 1..64, documented "The category of the attribute."), createdAt (@required), updatedAt (@required). CreateBrandProfile output member attributesCreated is documented as "The number of default attributes that were created for the brand profile." Recorded behavior: creating a brand profile seeds the service's full default attribute template, and the subsequent list returns that whole template -- 25 summaries, each one carrying a `category` drawn from the service's supported category values (the recording shows e.g. BUSINESS_ADDRESS, BUSINESS_IDENTITY, BUSINESS_CONTACT) and a human-readable attributeName containing spaces (e.g. "Address Line 2", "Business Type", "Company Name", "Contact Email"), ordered by attributeName. The count, the names and the categories are the service's fixed default template, not values from any one request.
Emulator: returns only 4 summaries, with placeholder single-token names and no `category` member at all -- {attributeName: BrandLogo, attributeType: IMAGE}, {CompanyAddress, TEXT}, {CompanyName, TEXT}, {ComplianceDocument, DOCUMENT} -- i.e. elements [4]..[24] of the expected list are absent and every element present differs in attributeName and lacks `category`. This is the same seeded-template defect that CreateBrandProfile reports as attributesCreated=4 instead of 25 (owned for CreateBrandProfile by 20261104T183251Z-endusermessaging-brand-profile-success-bodies-diverge-nanosecond-precision.md); the list projection of it is not covered by any open issue.
Why: a client enumerating a new brand profile's attributes sees a 4-element set whose names do not exist on AWS, so any code that looks up a default attribute by its real name ("Company Name", "Contact Email") finds nothing, and any code that groups or filters attributes by `category` sees the field unset on every element and cannot classify them. Pagination behavior is also wrong, since 25 items against @length max 100 / maxResults is a different page shape than 4.
Fix: the brand-profile default-attribute seed in the service's state (the template CreateBrandProfile installs and ListBrandProfileAttributes projects), plus the BrandProfileAttributeSummary serializer, which must emit `category` for every seeded attribute.
Traces: endusermessaging-brand-profile-lifecycle#task#4.

Cause 2 of 2: the ValidationException message for an unissued pagination token is re-worded and echoes the token. Fixture endusermessaging-brand-profile-identifier-and-list-errors, step task#6.
AWS: Smithy models ListBrandProfileAttributes.nextToken as NextToken (@length 1..2048, pattern ^.+$, "Opaque pagination token") and binds ValidationException (httpError 400, members message @required + optional fieldList); the model does not state the wording, so: recorded behavior only. The recording shows HTTP 400 with message exactly "Invalid or expired pagination token" and no fieldList. That sentence is entirely fixed text -- it has no variable parts, and in particular AWS does not echo the rejected token back to the caller.
Emulator: answers 400 but with message "nextToken forged-token is not a pagination token this service issued." -- different fixed text, and it interpolates the caller-supplied token value, which is not part of AWS's template.
Why: a client that branches on the error text (retry from the first page vs. surface the failure) does not match AWS's sentence and takes the wrong branch; log- and alarm-matching on the AWS wording never fires. Echoing the token also makes the message vary per request where AWS's does not.
Fix: the ListBrandProfileAttributes pagination-token validation path -- the ValidationException message it constructs.
Traces: endusermessaging-brand-profile-identifier-and-list-errors#task#6.

## Evidence
Cause 1 (endusermessaging-brand-profile-lifecycle#task#4, ListBrandProfileAttributes, expected 200, got 200):
  expected $.brandProfileAttributes = 25 summaries, each {attributeName, attributeType, category, createdAt, updatedAt}, e.g.
    [0] {attributeName: 'Address Line 2', attributeType: 'TEXT', category: 'BUSINESS_ADDRESS'}
    [1] {attributeName: 'Business Type', attributeType: 'TEXT', category: 'BUSINESS_IDENTITY'}
    [2] {attributeName: 'Business Vertical', attributeType: 'TEXT', category: 'BUSINESS_IDENTITY'}
    [3] {attributeName: 'City', attributeType: 'TEXT', category: 'BUSINESS_ADDRESS'}
    [4] {attributeName: 'Company Address', attributeType: 'TEXT', category: 'BUSINESS_ADDRESS'}
    [5] {attributeName: 'Company Name', attributeType: 'TEXT', category: 'BUSINESS_IDENTITY'}
    [6] {attributeName: 'Contact Email', attributeType: 'TEXT', category: 'BUSINESS_CONTACT'} ... through [24]
  actual   $.brandProfileAttributes = 4 summaries, no `category` member on any of them:
    [0] {attributeName: 'BrandLogo', attributeType: 'IMAGE'}
    [1] {attributeName: 'CompanyAddress', attributeType: 'TEXT'}
    [2] {attributeName: 'CompanyName', attributeType: 'TEXT'}
    [3] {attributeName: 'ComplianceDocument', attributeType: 'DOCUMENT'}
    [4]..[24] = None (absent)
  (createdAt/updatedAt were shaped-not-asserted in this cluster; their precision defect is already owned elsewhere.)

Cause 2 (endusermessaging-brand-profile-identifier-and-list-errors#task#6, ListBrandProfileAttributes, expected 400, got 400):
  expected $.message = "Invalid or expired pagination token"
  actual   $.message = "nextToken forged-token is not a pagination token this service issued."
  Status and error type agree; only the message text diverges.

## Steps to Reproduce
Cause 1: replay fixture endusermessaging-brand-profile-lifecycle. Step task#1 CreateBrandProfile, then step task#4 GET /v1/brand-profiles/{brandProfileId}/attributes (ListBrandProfileAttributes). Compare $.brandProfileAttributes against the recorded 25-element default template: expect 25 elements each carrying `category`; the emulator returns 4 elements with different attributeNames and no `category`.

Cause 2: replay fixture endusermessaging-brand-profile-identifier-and-list-errors, step task#6. Call ListBrandProfileAttributes on an existing brand profile with nextToken set to a token the service never issued (the fixture uses a forged value). Expect HTTP 400 with $.message exactly "Invalid or expired pagination token"; the emulator returns 400 with a re-worded message that interpolates the supplied token.
