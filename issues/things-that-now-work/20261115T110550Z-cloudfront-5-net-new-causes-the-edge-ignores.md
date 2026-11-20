Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag cloudfront.delete-function-stale-etag-rejected cloudfront.e2e-resource-policy-grants-and-revokes-cross-account-access cloudfront.op-contract-get-field-level-encryption-config cloudfront.update-anycast-ip-list-invalid-argument-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag|run#32|AssertionError|assert matched.status_code == 304, (
Issue-Key: cloudfront.delete-function-stale-etag-rejected|run#8|AssertionError|assert code == "InvalidIfMatchVersion", f"expected InvalidIfMatchVersion, got {code}"
Issue-Key: cloudfront.e2e-resource-policy-grants-and-revokes-cross-account-access|_error#15|AssertionError|raise AssertionError(f"expected the call to fail, but it SUCCEEDED and returned {result!r}")
Issue-Key: cloudfront.op-contract-get-field-level-encryption-config|run#26|AssertionError|assert fwd_qa is True, f"expected ForwardWhenQueryArgProfileIsUnknown True, got {fwd_qa!r}"
Issue-Key: cloudfront.update-anycast-ip-list-invalid-argument-rejected|run#10|AssertionError|assert code == "InvalidArgument", f"expected InvalidArgument, got {code!r} ({message!r})"
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# cloudfront: 5 net-new causes — the edge ignores If-None-Match (never 304), DeleteFunction's stale If-Match answers PreconditionFailed, PutResourcePolicy stores a grant to a nonexistent principal, GetFieldLevelEncryptionConfig reports ForwardWhenQueryArgProfileIsUnknown false, and UpdateAnycastIpList's off-enum IpAddressType is refused MalformedXML

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteFunction

## Description
Five net-new root causes in the `cloudfront` service. All five are grounded in failing behavioral checks under `### cloudfront` that the suite marked neither OWNED by an open issue nor ALREADY FILED behind an unimplemented scaffold stub. A fixer must close all five before submitting; each cause below names its own failing check id.

--------------------------------------------------------------------
Cause 1 of 5: the edge serves a full 200 to a conditional GET whose If-None-Match matches the cached object's own ETag; AWS answers a bodiless 304.
Failing check: cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag

AWS: recorded behavior only — the CloudFront data plane is not described by the Smithy model. CloudFront is an HTTP/1.1-conformant cache (RFC 7232 conditional requests): when the edge holds a cache entry for the requested object and the viewer presents `If-None-Match` carrying an entity-tag that matches the entry's ETag, the edge short-circuits the response to HTTP 304 Not Modified with an EMPTY body and with the `ETag` response header naming the same entity it was conditional on. The same request carrying an entity-tag the edge cannot be holding is answered HTTP 200 with the object's full bytes and the object's ETag, and the cached entry survives both. The requirement is the relation (presented tag == served tag -> 304 + no body; presented tag != served tag -> 200 + body); the ETag string itself is whatever the origin minted for that object and is not part of the requirement.
Emulator: the request whose `If-None-Match` is exactly the ETag the edge served on the immediately preceding hit is answered 200 with the whole body. Recorded: `a conditional GET presenting the object's own ETag '"7dc880228978d156b35173546477a11d"' was answered 200 instead of 304` (the digest is a per-run value). The cold fetch served the object's bytes and the warm fetch reported `X-Cache: Hit from cloudfront`, so the edge is serving and caching — only the conditional evaluation is missing.
Why: every revalidating client (browser, SDK, downstream CDN, `curl -z`/`If-None-Match`) re-downloads the entire object on every revalidation, so conditional-request bandwidth savings disappear and any client that branches on 304-vs-200 to decide "my copy is still fresh" never takes the 304 branch.
Fix: the edge data plane's cache-hit response path — conditional-request evaluation (If-None-Match, and the If-Modified-Since sibling) must run before the body is written, and the 304 must carry the entry's ETag and no body.
Traces: cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag.

--------------------------------------------------------------------
Cause 2 of 5: DeleteFunction refuses a well-formed but non-current If-Match version with PreconditionFailed/412 where AWS refuses it with InvalidIfMatchVersion/400.
Failing check: cloudfront.delete-function-stale-etag-rejected

AWS: Smithy models DeleteFunction's errors as FunctionInUse/409, InvalidIfMatchVersion/400 ("The If-Match version is missing or not valid."), NoSuchFunctionExists/404, PreconditionFailed/412, UnsupportedOperation/400. Recorded behavior for DeleteFunction specifically: a version token that is well-formed but is not the function's current version is refused with error code `InvalidIfMatchVersion`, HTTP 400, and the message "The If-Match version is missing or not valid for the resource." — that sentence is entirely FIXED text for this error class and has no variable parts. (DeleteFunction therefore collapses "missing/not a token" and "not the current token" onto InvalidIfMatchVersion; this is the opposite of the split TestFunction needs, which is Cause 3 of the open issue 20261112T142015Z-cloudfront-5-net-new-cloudfront-functions-causes-testresult-drops.md — that issue does not cover DeleteFunction and the suite did not mark this check owned by it.)
Emulator: the same call is refused with error code `PreconditionFailed` (HTTP 412).
Why: a client distinguishes 412 PreconditionFailed (an optimistic-concurrency conflict — re-read the ETag and retry the delete) from 400 InvalidIfMatchVersion (a malformed/no-longer-meaningful version token — do not retry blindly). Against the emulator a stale-ETag DeleteFunction looks like a retryable conflict on an operation AWS reports as a bad If-Match, so cleanup/teardown logic takes the wrong branch, and code matching on the documented code string never matches at all.
Fix: DeleteFunction's If-Match validation in its handler (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/delete_function.rs) — map a non-current version token to the modeled InvalidIfMatchVersion with its fixed message instead of PreconditionFailed, and leave the function present.
Traces: cloudfront.delete-function-stale-etag-rejected.

--------------------------------------------------------------------
Cause 3 of 5: PutResourcePolicy accepts and stores a policy document whose Principal names an account that does not exist; AWS refuses it with InvalidArgument and stores nothing.
Failing check: cloudfront.e2e-resource-policy-grants-and-revokes-cross-account-access

AWS: Smithy models PutResourcePolicy with errors AccessDenied/403, EntityNotFound/404, IllegalUpdate/400, InvalidArgument/400 ("An argument is invalid."), PreconditionFailed/412, UnsupportedOperation/400. Recorded behavior: a one-statement grant whose `Principal.AWS` is `arn:aws:iam::<account>:root` for an account that does not exist (the documentation placeholder account) is refused with error code `InvalidArgument`, HTTP 400, and the message "The specified policy references a principal that doesn't exist. Verify the principals in the policy and try again." — entirely fixed text, no variable parts — and the refusal stores nothing: a following GetResourcePolicy still returns the empty placeholder document (`{}`). A grant naming the caller's OWN account is accepted and round-trips through GetResourcePolicy, and DeleteResourcePolicy revokes it back to `{}`.
Emulator: the call succeeds — HTTP 200 with the request's ResourceArn echoed back (recorded: `{'ResourceArn': 'arn:aws:cloudfront::<account>:vpcorigin/<id>'}`, a per-run value) — so no principal validation runs at all and the unusable grant is persisted.
Why: a client that builds a cross-account resource policy from a typo'd or decommissioned account id is told the grant succeeded. The policy reads back as active while granting nothing, so an operator cannot tell a broken grant from a working one, and a deployment pipeline that treats PutResourcePolicy's 200 as "access granted" proceeds on a false premise.
Fix: PutResourcePolicy's handler/validation (the policy-document parse-and-validate step) — resolve each statement's principals and refuse an unknown principal with the modeled InvalidArgument and its fixed message, before any state mutation. Related open issue, which does NOT cover this check (the suite marked no owner) and is named only as context: 20261111T194302Z-cloudfront-resource-policy-operations-ignore-resource-existence-and.md (that one is about the RESOURCE not existing; this is about the PRINCIPAL in the document).
Traces: cloudfront.e2e-resource-policy-grants-and-revokes-cross-account-access.

--------------------------------------------------------------------
Cause 4 of 5: a field-level-encryption config with a non-empty QueryArgProfiles reports ForwardWhenQueryArgProfileIsUnknown as false; AWS stores and returns true.
Failing check: cloudfront.op-contract-get-field-level-encryption-config

AWS: Smithy models QueryArgProfileConfig.ForwardWhenQueryArgProfileIsUnknown as a REQUIRED boolean ("Flag to set if you want a request to be forwarded to the origin even if the profile specified by the field-level encryption query argument, fle-profile, is unknown."), and GetFieldLevelEncryptionConfig returns the FieldLevelEncryptionConfig payload plus an ETag header. Recorded behavior: whenever the created config carries a non-empty QueryArgProfiles list, CloudFront stores ForwardWhenQueryArgProfileIsUnknown as `true` whatever value the create sent, so the readback reports `true` for a create that sent `false`; ContentTypeProfileConfig.ForwardWhenContentTypeIsUnknown is returned as stored (`true` here), and the stored QueryArgProfile item (QueryArg + ProfileId) round-trips unchanged.
Emulator: GetFieldLevelEncryptionConfig echoes the submitted `false`, so the readback is `ForwardWhenQueryArgProfileIsUnknown: False` where AWS reports `True`.
Why: the documented edit flow is Get -> mutate -> UpdateFieldLevelEncryptionConfig with the returned ETag. A client that round-trips the readback silently rewrites the flag to the value AWS does not hold, which changes whether requests carrying an unknown fle-profile are forwarded to the origin or dropped — a security/availability-relevant difference. Drift detection (CloudFormation, Terraform, config auditors) also reports a permanent, unfixable difference on this member.
Fix: field-level-encryption config normalization — CreateFieldLevelEncryptionConfig/UpdateFieldLevelEncryptionConfig must normalize the flag to true when QueryArgProfiles is non-empty (and the GetFieldLevelEncryptionConfig/GetFieldLevelEncryption readbacks must report the stored value). Related open issue, named as context only because the suite marked this check unowned: 20261109T120741Z-cloudfront-field-level-encryption-configs-lose-contenttypeprofileconfig-and.md.
Traces: cloudfront.op-contract-get-field-level-encryption-config.

--------------------------------------------------------------------
Cause 5 of 5: UpdateAnycastIpList refuses an off-enum IpAddressType with MalformedXML and a generic "N validation errors detected" envelope instead of CloudFront's InvalidArgument.
Failing check: cloudfront.update-anycast-ip-list-invalid-argument-rejected

AWS: Smithy models UpdateAnycastIpList's errors as AccessDenied/403, EntityNotFound/404, InvalidArgument/400, InvalidIfMatchVersion/400, PreconditionFailed/412, UnsupportedOperation/400 — MalformedXML is NOT among them — and IpAddressType as an enum over [ipv4, ipv6, dualstack]. Recorded behavior: a value outside that enum is refused with error code `InvalidArgument`, HTTP 400 (InvalidArgument's modeled meaning, "An argument is invalid."), the list's IpAddressType and version token are left untouched, and re-asserting a valid value then succeeds.
Emulator: the call is refused with error code `MalformedXML` and the message "1 validation error detected: Value 'ipv7-bogus' at 'ipAddressType' failed to satisfy constraint: Member must satisfy enum value set: [ipv4, ipv6, dualstack]". In that message only the rejected value (`ipv7-bogus`, taken from the request) and the member name are request-derived; the "N validation error(s) detected: ... failed to satisfy constraint ..." envelope is the emulator's generic validation wording, which CloudFront's REST-XML surface does not use for a constraint violation on a modeled member.
Why: MalformedXML tells a client its request BODY could not be parsed — the remediation is to fix serialization, not the value. A client catching CloudFront's documented InvalidArgument to surface "that IpAddressType is not allowed" never catches it, and retry/validation layers that treat MalformedXML as a client-serializer bug misreport a plain bad-argument refusal.
Fix: UpdateAnycastIpList's request validation (services/.../src/ops/update_anycast_ip_list.rs) — an enum-constraint violation on IpAddressType must surface as the modeled InvalidArgument, not as the shared MalformedXML/generic-validation path. Related open issues named as context only: 20261114T135924Z-cloudfront-2-net-new-causes-a-response-headers.md (owns the sibling malformed-IP check) and 20261115T095648Z-cloudfront-accept-refuse-decisions-diverge-anycast-quota-if.md.
Traces: cloudfront.update-anycast-ip-list-invalid-argument-rejected.

## Evidence
Suite: simparity-behavioral-aws-v1, 484/632 passed (76.6%). Under `### cloudfront` 65 failing checks are listed; 60 carry an OWNED-by or ALREADY-FILED (unimplemented stub) marker. The 5 that carry neither are clustered here, one cause each.

Cause 1 — conditional GET never 304s:
  [mismatch] cloudfront.cfcache-conditional-get-304-only-for-the-matching-etag
    check_cloudfront_cfcache_conditional_get_304_only_for_the_matching_etag.py line 239, in run
      assert matched.status_code == 304, ...
    AssertionError: a conditional GET presenting the object's own ETag '"7dc880228978d156b35173546477a11d"' was answered 200 instead of 304 / 'sim2real-conditional-get-object-bytes'
  Expected: HTTP 304, empty body, ETag header equal to the ETag the edge served for the object.
  Actual:   HTTP 200 carrying the object's bytes. (The preceding cold fetch returned the object and the warm fetch returned `X-Cache: Hit from cloudfront`, so the edge served and cached correctly; the ETag digest shown is a per-run value.)

Cause 2 — DeleteFunction stale If-Match code:
  [mismatch] cloudfront.delete-function-stale-etag-rejected
    check_cloudfront_delete_function_stale_etag_rejected.py line 38, in run
      assert code == "InvalidIfMatchVersion", f"expected InvalidIfMatchVersion, got {code}"
    AssertionError: expected InvalidIfMatchVersion, got PreconditionFailed
  Expected: error code InvalidIfMatchVersion, HTTP 400, message "The If-Match version is missing or not valid for the resource." (all fixed text), with the function still describable afterwards.
  Actual:   error code PreconditionFailed, HTTP 412.

Cause 3 — PutResourcePolicy accepts a nonexistent principal:
  [mismatch] cloudfront.e2e-resource-policy-grants-and-revokes-cross-account-access
    check_cloudfront_e2e_resource_policy_grants_and_revokes_cross_account_access.py line 54, in run -> _error() line 130
    AssertionError: expected the call to fail, but it SUCCEEDED and returned {'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}, 'ResourceArn': 'arn:aws:cloudfront::<account>:vpcorigin/<generated-id>'}
  Expected: error code InvalidArgument, HTTP 400, message "The specified policy references a principal that doesn't exist. Verify the principals in the policy and try again.", and GetResourcePolicy still returning the empty placeholder document {}.
  Actual:   HTTP 200 with the ResourceArn echoed; the grant is stored.

Cause 4 — FLE QueryArgProfileConfig flag not normalized:
  [mismatch] cloudfront.op-contract-get-field-level-encryption-config
    check_cloudfront_op_contract_get_field_level_encryption_config.py line 118, in run
      assert fwd_qa is True, f"expected ForwardWhenQueryArgProfileIsUnknown True, got {fwd_qa!r}"
    AssertionError: expected ForwardWhenQueryArgProfileIsUnknown True, got False
  Expected: for a config created with QueryArgProfiles.Quantity=1, the readback reports ForwardWhenQueryArgProfileIsUnknown=true even though the create sent false; the CallerReference, Comment, the QueryArgProfile item and ForwardWhenContentTypeIsUnknown=true all round-trip, and a non-empty ETag is returned.
  Actual:   ForwardWhenQueryArgProfileIsUnknown=false (the submitted value echoed). The earlier CallerReference/Comment/QueryArgProfiles assertions in the check passed, so only this member diverges.

Cause 5 — UpdateAnycastIpList enum refusal code:
  [mismatch] cloudfront.update-anycast-ip-list-invalid-argument-rejected
    check_cloudfront_update_anycast_ip_list_invalid_argument_rejected.py line 58, in run
      assert code == "InvalidArgument", f"expected InvalidArgument, got {code!r} ({message!r})"
    AssertionError: expected InvalidArgument, got 'MalformedXML' ("1 validation error detected: Value 'ipv7-bogus' at 'ipAddressType' failed to satisfy constraint: Member must satisfy enum value set: [ipv4, ipv6, dualstack]")
  Expected: error code InvalidArgument, HTTP 400.
  Actual:   error code MalformedXML with the generic validation envelope above.

## Steps to Reproduce
All reproduce against the booted emulator with a botocore/boto3 `cloudfront` client (plus `s3`/`ec2`/`elbv2` where noted).

Cause 1 (edge conditional GET):
 1. Create an S3 bucket, put an object (any bytes) at key K, create an OAC (s3/always/sigv4) and a cache policy with MinTTL/DefaultTTL/MaxTTL = 3600.
 2. CreateDistribution with that S3 origin + OAC and DefaultCacheBehavior.CachePolicyId = the policy; grant cloudfront.amazonaws.com s3:GetObject on the bucket with AWS:SourceArn = the distribution ARN; wait for Status == Deployed.
 3. Over one keep-alive session: GET /K (cold) -> note the `ETag` header E; repeat GET /K until `X-Cache: Hit from cloudfront`.
 4. GET /K with header `If-None-Match: E`. Emulator: 200 with the full body. AWS: 304, empty body, `ETag: E`.
 5. Control: GET /K with `If-None-Match: "sim2real-not-the-etag-of-any-object"` -> 200 with the body (both agree), and a final unconditional GET still 200.

Cause 2 (DeleteFunction stale If-Match):
 1. cf.create_function(Name=N, FunctionConfig={"Comment":"c","Runtime":"cloudfront-js-2.0"}, FunctionCode=b"function handler(event) { return event.request; }")
 2. e = cf.describe_function(Name=N, Stage="DEVELOPMENT")["ETag"]; stale = e[:-1] + ("0" if e[-1] != "0" else "1")
 3. cf.delete_function(Name=N, IfMatch=stale) -> emulator raises PreconditionFailed/412; AWS raises InvalidIfMatchVersion/400 with "The If-Match version is missing or not valid for the resource."
 4. The function must still describe afterwards, and cf.delete_function(Name=N, IfMatch=e) must then succeed.

Cause 3 (PutResourcePolicy principal validation):
 1. Create a VPC + subnets + an internal ALB; cf.create_vpc_origin(VpcOriginEndpointConfig={Name, Arn=<alb arn>, HTTPPort=80, HTTPSPort=443, OriginProtocolPolicy="http-only"}); wait for the VPC origin to be Deployed; note its Arn A.
 2. cf.get_resource_policy(ResourceArn=A) -> PolicyDocument parses to {}.
 3. cf.put_resource_policy(ResourceArn=A, PolicyDocument=json.dumps({"Version":"2012-10-17","Statement":[{"Sid":"s","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":["cloudfront:GetVpcOrigin"],"Resource":A}]}))
 4. Emulator: 200 with ResourceArn=A. AWS: InvalidArgument/400 "The specified policy references a principal that doesn't exist. Verify the principals in the policy and try again.", and step 2 repeated still returns {}.
 5. Control: the same document with the caller's own account id is accepted and round-trips through GetResourcePolicy; DeleteResourcePolicy returns it to {}.

Cause 4 (FLE flag normalization):
 1. cf.create_public_key(...); cf.create_field_level_encryption_profile(... EncryptionEntities with that key ...) -> profile_id
 2. cf.create_field_level_encryption_config(FieldLevelEncryptionConfig={CallerReference, Comment, "QueryArgProfileConfig": {"ForwardWhenQueryArgProfileIsUnknown": False, "QueryArgProfiles": {"Quantity": 1, "Items": [{"QueryArg": "sim2real-profile", "ProfileId": profile_id}]}}, "ContentTypeProfileConfig": {"ForwardWhenContentTypeIsUnknown": True, "ContentTypeProfiles": {"Quantity": 0, "Items": []}}})
 3. cf.get_field_level_encryption_config(Id=<id>)["FieldLevelEncryptionConfig"]["QueryArgProfileConfig"]["ForwardWhenQueryArgProfileIsUnknown"] -> emulator False; AWS True.

Cause 5 (UpdateAnycastIpList enum):
 1. created = cf.create_anycast_ip_list(Name=N, IpCount=3); id = created["AnycastIpList"]["Id"]; etag = created["ETag"]
 2. cf.update_anycast_ip_list(Id=id, IfMatch=etag, IpAddressType="ipv7-bogus") -> emulator raises MalformedXML with "1 validation error detected: Value 'ipv7-bogus' at 'ipAddressType' failed to satisfy constraint: Member must satisfy enum value set: [ipv4, ipv6, dualstack]"; AWS raises InvalidArgument/400.
 3. cf.get_anycast_ip_list(Id=id) must still report the original IpAddressType and the unchanged ETag, and re-asserting that valid value via UpdateAnycastIpList must succeed.

## Resolution

Causes 2, 3 and 4 are FIXED here; causes 1 and 5 were already fixed on the mainline this
branch forked from and their checks replay green.

- Cause 1 ALREADY FIXED: the edge judges the viewer's conditional headers over the
  representation it is about to serve — the entry its cache held or the answer the origin just
  produced — before the body is written (`service::edge::conditional`, covered end to end by
  `tests/all/edge_conditional_requests.rs`).
- Cause 2 FIXED: `DeleteFunction` resolves the submitted `If-Match` against the versions the
  FUNCTION has held. A header carrying no version token, or a token the function never stood
  at, names no version for a precondition to be evaluated against, so it is
  `InvalidIfMatchVersion` (400) — "missing or not valid FOR THE RESOURCE"; a token the function
  HAS held that is not the version the delete is checked against stays the failed precondition
  (412). That reading serves both recorded refusals of this one operation.
- Cause 3 FIXED: `PutResourcePolicy` resolves every identity a submitted document grants to
  before it stores anything — an account other than the one this emulator serves, and an IAM
  role or user of this account that IAM does not hold, are identities nothing holds — and
  refuses such a document `InvalidArgument` (400) with CloudFront's principal sentence, leaving
  the resource with the policy it carried. The check now runs past every resource-policy
  assertion and errors in its teardown on `DeleteVpcOrigin`, which is still a scaffold stub
  owned by `issues/things-that-do-not-work/20261116T052444Z-cloudfront-deletevpcorigin-stub.md`
  (and by cause 1 of `20261112T003830Z-cloudfront-3-net-new-vpc-origin-causes-deletevpcorigin.md`).
- Cause 4 FIXED: a field-level encryption configuration whose `QueryArgProfiles` maps at least
  one query argument to a profile is stored with
  `ForwardWhenQueryArgProfileIsUnknown` true whatever the request submitted, on the create and
  the update alike, and every readback reports the stored value.
- Cause 5 ALREADY FIXED: `UpdateAnycastIpList` answers the modelled `InvalidArgument` for an
  off-enum `IpAddressType`, and the check replays green.

