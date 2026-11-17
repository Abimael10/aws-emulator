Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: endusermessaging.list-tags-for-resource-rejects-arn-from-another-region
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging.list-tags-for-resource-rejects-arn-from-another-region|run#10|AssertionError|assert message == (
Issue-Commit: eab59dd3d13731329d6515ebc512ab3051ad49e1

# endusermessaging ListTagsForResource cross-region ARN refusal is re-worded as an "Invalid ARN ..." sentence instead of the service's partition/region mismatch message

> _Note: this issue was identified against the code base at commit `eab59dd3d13731329d6515ebc512ab3051ad49e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
1 net-new cause in this service's tagging read path, observed through ListTagsForResource (GET /v1/tags/{resourceArn}). The open issue 20261117T042000Z-endusermessaging-tag-reads-diverge-tag-writes-never-reach.md describes the same neighbourhood (its Cause 4 is the wire-side sighting of this wording), but it carries only wire replay keys for ListTagsForResource and the replay report marks this behavioral check neither owned nor stub-blocked, so the check is filed here and named against that issue rather than assumed covered. Fixing one should close both; this issue exists so the behavioral check id is carried somewhere.

Cause 1 of 1: ListTagsForResource refuses a resourceArn scoped to another Region with the right status and error code but a message this API does not emit.
AWS: Smithy (com.amazonaws.endusermessaging#EndUserMessaging, restJson1): ListTagsForResource GET /v1/tags/{resourceArn}, resourceArn @required @httpLabel AmazonResourceName (@length 20..256, pattern ^arn:[A-Za-z0-9_:/-]+$), errors include ValidationException (HTTP 400, httpErrorDefaulted) and ResourceNotFoundException (404). The model does not state the wording, so: recorded behavior only - a syntactically well-formed ARN of this service whose partition/Region fields do not match the scope the request was made in is refused 400 ValidationException with the message "ARN partition/region mismatch: expected partition '<expected-partition>' region '<expected-region>', got partition '<arn-partition>' region '<arn-region>'". Fixed text: "ARN partition/region mismatch: expected partition '", "' region '", "', got partition '", "' region '", and the trailing "'". Variable parts: the expected partition and Region, taken from the partition/Region the request was made in, and the partition and Region parsed out of the request's own resourceArn. The 400 status and the ValidationException code are requirements too; they already agree.
Emulator: 400 ValidationException, but the message is "Invalid ARN: <resourceArn> does not name a resource in Region <request-region>, the Region this request was made in" - a different sentence that quotes the whole ARN (an account id and a generated resource id included) and never names the expected-vs-actual partition and Region as separate, parseable tokens. The emulator also only compares the Region field and never the partition, so it has no value to report for the partition half of the message.
Why: a client that distinguishes a cross-partition/cross-Region ARN from a merely malformed one by the refusal text - log and alarm assertions, a user-facing error surface, an SDK-level string match, or a test suite pinned to AWS's wording - reads a sentence AWS never emits and cannot recover the two facts it needs (which partition/Region was expected, which the ARN named). Because the status and code match, the divergence is invisible until the message is parsed.
Fix: the tagging state module's cross-region refusal constructor for this service - the one that words the ValidationException for an ARN resolved outside the request's scope - must emit the service's partition/region mismatch message, naming both the expected and the ARN-supplied partition and Region, and must compare the partition as well as the Region; the shared ValidationExceptionField entry on /resourceArn must follow the same wording.
Traces: behavioral check endusermessaging.list-tags-for-resource-rejects-arn-from-another-region.

## Evidence
Behavioral check endusermessaging.list-tags-for-resource-rejects-arn-from-another-region [mismatch].

The check creates a brand profile with tags, rewrites the Region segment of the returned brandProfileArn to a different Region, and calls ListTagsForResource with that ARN.

Error code: expected ValidationException, got ValidationException (agrees).
Message: mismatch.
  expected: "ARN partition/region mismatch: expected partition 'aws' region '<request-region>', got partition 'aws' region '<other-region>'"
  actual:   "Invalid ARN: arn:aws:end-user-messaging:<other-region>:<account>:brand-profile/<profile-id> does not name a resource in Region <request-region>, the Region this request was made in"

AssertionError from the replay: unexpected message 'Invalid ARN: arn:aws:end-user-messaging:us-east-1:000000000000:brand-profile/bp-7ad638a9d00000001 does not name a resource in Region us-west-2, the Region this request was made in'.

In that quoted pair, 'aws', the two Region tokens, the account id and the brand-profile id are per-run values filling the template - the requirement is the fixed text and the rule that the expected partition/Region come from the request's scope and the actual ones from the request's resourceArn.

Source of the emulator wording: services/service-com-amazonaws-endusermessaging-endusermessaging/src/state/tags.rs:78-95 (cross_region_resource_arn), reached from resolve_tagged_resource at src/state/tags.rs:128-133 (which tests arn.region != region only, never the partition); the read handler is services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/list_tags_for_resource.rs.

## Steps to Reproduce
1. POST /v1/brand-profiles (CreateBrandProfile) with brandProfileName and tags [{"key":"team","value":"alpha"}]; note brandProfileArn and brandProfileId.
2. Rewrite the Region segment of brandProfileArn to a different Region (e.g. us-east-1 when the request Region is us-west-2), leaving every other segment alone.
3. GET /v1/tags/{that rewritten ARN} (ListTagsForResource).
   Expected: 400 ValidationException, message "ARN partition/region mismatch: expected partition 'aws' region '<request-region>', got partition 'aws' region '<other-region>'".
   Actual: 400 ValidationException, message "Invalid ARN: <rewritten ARN> does not name a resource in Region <request-region>, the Region this request was made in".
4. Positive control: GET /v1/tags/{original brandProfileArn} must answer 200 with the create-time tags.
Replay: behavioral check endusermessaging.list-tags-for-resource-rejects-arn-from-another-region.
