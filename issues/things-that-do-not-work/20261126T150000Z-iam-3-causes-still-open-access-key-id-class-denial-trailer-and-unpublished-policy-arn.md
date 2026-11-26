Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.iam-access-key-crud iam.user-access-key-round-trips-status-and-never-used iam.requested-region-for-global-iam-is-us-east-1-other-region-denied iam.op-contract-attach-user-policy
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.iam-access-key-crud|run#9|AssertionError|assert key_id.startswith("AKIA"), f"expected AKIA-prefixed access key id, got {key_id!r}"
Issue-Key: iam.user-access-key-round-trips-status-and-never-used|run#6|AssertionError|assert key["AccessKeyId"].startswith("AKIA"), f"a long-term key id starts AKIA: {key['AccessKeyId']!r}"
Issue-Key: iam.requested-region-for-global-iam-is-us-east-1-other-region-denied|_assert_stably_denied#18|AssertionError|assert msg == want, f"{action}: AccessDenied message: expected {want!r}, got {msg!r}"
Issue-Key: iam.op-contract-attach-user-policy|_error#10|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Commit: 496f13e8c3207ef947f41f94a4f2c11ee1d8339b

# iam: 3 causes still open — the minted access-key id class is a core-owned value, the authorization-details trailer is recorded both ways for one denial class, and refusing an unpublished AWS-managed PolicyArn needs the catalog to be authoritative

> _Split out of `issues/things-that-now-work/20261124T000913Z-iam-4-net-new-causes-access-key-ids.md` (its causes 1, 2 and 4), whose cause 3 — the AWS-managed policy catalog missing `AmazonDynamoDBReadOnlyAccess` — is fixed and whose replay key `iam|GetPolicy|NoSuchEntity` now passes. These three replay keys do NOT pass, so they are re-filed here rather than recorded as working. A later fixer starts from this analysis instead of from scratch._

## Operation
CreateAccessKey

## Description
Three causes, each with the analysis that was done for it and what each one is blocked on.

=== Cause 1 of 3: minted IAM access-key ids are `EMULATOR`-prefixed rather than in AWS's `AKIA` class — BLOCKED on a read-only `vendor/` file ===
AWS: a long-term IAM user access key id is 20 characters beginning with the literal `AKIA` (IAM User
Guide, "IAM identifiers" -> unique ID prefixes; a temporary STS credential begins `ASIA`). The
Smithy model constrains only `accessKeyIdType` (@length 16-128, @pattern `^[\w]+$`), so the class
token is AWS's documented behaviour. The requirement is the FORM (`AKIA` plus 16 further
characters); the characters after the class token are per-mint values and are not part of it.
Emulator: `CreateAccessKey` answers 200 with an id beginning `EMULATORAK`, which `ListAccessKeys`
and `GetAccessKeyLastUsed` project unchanged.
Why it is BLOCKED, and on what — a service crate cannot choose the class:
* `vendor/server/src/aws_credentials.rs:47` — `AwsAccessKey::mint` builds the id as
  `aws_key_with_secret("EMULATORAK")`, and the comment at :12-16 states the `EMULATOR` class is
  deliberate "so secret scanners never mistake one for an AWS key";
* `vendor/server/src/credential_store.rs:17` — `pub trait CredentialKind: Send + Sync +
  sealed::Sealed` with `sealed` private to the core, and `CredentialStore::create` (:33) taking only
  `&'static dyn CredentialKind`, so no crate under `services/` can define a kind of its own;
* `vendor/server/src/request_authentication.rs:138-142` — a signed request is verified against the
  id the credential store MINTED, so an id a handler constructs instead would make the key the
  operation hands back unusable for signing while `--auth on`; `AGENTS.md` forbids a handler
  constructing a credential id or secret for exactly this reason.
The required core change: `AwsAccessKey::mint` (and `AwsSession::mint` with it) must mint AWS's
published classes — `AKIA` for a long-term key, `ASIA` for a temporary one — or `CredentialKind`
must be opened so the service that mints names the class. After it, `CreateAccessKey` needs no
change at all. This is the same core change the already-filed family waits on:
`issues/things-that-do-not-work/20261026T234020Z-cognito-identity-a-web-identity-session-still-vends-an-emul-class-accesskeyid.md`
(the fullest statement of the blocker),
`20261116T105641Z-iam-7-net-new-causes-minted-access-key.md`,
`20261115T051545Z-sts-assumerole-inside-a-function-still-vends-an-emul-class-accesskeyid.md` and the
other `sts`/`cognito-identity` reports listed there — ONE core change closes all of them together.
Traces: iam.iam-access-key-crud, iam.user-access-key-round-trips-status-and-never-used.

=== Cause 2 of 3: which AccessDenied messages carry the authorization-details trailer is not a function of the request in this corpus ===
AWS: recorded behaviour only (no model carries the message text). The denial sentence itself is
correct in the emulator; what differs is whether the second sentence `. Go to
https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<id> for
complete details, or call the GetRequestAuthorizationDetails API with the following authorization
id: <id>` is appended.
Emulator: it is appended for every refusal except the one class
`crate::state::authorization::details::DeniedRequest::points_at_its_detail` withholds it for (a role
session's refusal for want of an identity-based grant where a document in force names the action).
Why it is OPEN rather than fixed here: the failing check requires the trailer on its
`iam:ListAccountAliases` leg (resource `*`) and NOT on its `iam:ListRoles` leg (resource
`arn:aws:iam::<account>:role/`) — and those two legs are the SAME denial class: a long-term IAM user
key, an `Allow` naming the action in force, and that `Allow`'s `aws:RequestedRegion` condition
unsatisfied, so nothing granted the call. The corpus records that class both ways elsewhere too:
`iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol`
requires the trailer for an implicit-deny `iam:ListUsers` on resource
`arn:aws:iam::<account>:user/` (see
`issues/things-that-now-work/20261117T135333Z-iam-secretsmanager-iam-accessdenied-message-omits-the-getrequestauthorizationdetails-pointer-iam.md`),
while `iam.op-contract-listUsers-missing-permission-denied` requires its ABSENCE for the same action
and the same resource; `iam.condition-key-flips-a-real-authorization-decision` requires it for a
condition-withheld `Allow`, which is precisely the ListRoles leg's class; and
`iam.op-contract-listGroups-missing-permission-denied` was recorded WITH the trailer in
`issues/things-that-now-work/20261027T032550Z-iam-5-net-new-behavioral-root-causes-authorization.md`
and WITHOUT it in
`issues/things-that-now-work/20261118T021609Z-iam-4-net-new-causes-denials-append-an.md`.
So no function of (deny reason, credential kind, grant read, action's resource) separates the two
families: the only thing that fits every unit is a table keyed on the ACTION the recorded call
named, which is not a rule and would be fitted to the recording. What this cause needs is the
recorded corpus itself: the per-denial-class table, read out of the recordings rather than inferred
from the four issues above. Until then a fix in either direction breaks the other family.
Traces: iam.requested-region-for-global-iam-is-us-east-1-other-region-denied.

=== Cause 3 of 3: AttachUserPolicy accepts a PolicyArn in AWS's namespace that names no published policy ===
AWS: `AttachUserPolicy` with a `PolicyArn` that resolves to no policy is refused HTTP 404 /
`NoSuchEntity` with `Policy <PolicyArn> does not exist or is not attachable.`, and nothing is
attached. The same holds for `AttachRolePolicy`, `AttachGroupPolicy` and the permissions-boundary
puts.
Emulator: the refusal path EXISTS and is correctly worded — every attach already calls
`ComAmazonawsIamAwsidentitymanagementv20100508State::managed_policy_exists`
(`services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state/account.rs:1966`) and
refuses with `crate::state::policy_not_attachable`. What makes the call succeed is the resolution
underneath it:
`services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state/managed_policy_catalog.rs`'s
`is_published_aws_managed_policy_arn` answers `true` for every name the built-in catalog does not
carry (`None => true`), deliberately: the catalog is a subset of AWS's thousand-plus policies, so
"a name the catalog does not carry cannot be judged" and the subset errs towards accepting an
attachment AWS accepts.
Why it is OPEN rather than fixed here: closing it means making the catalog AUTHORITATIVE for AWS's
namespace, which is a cross-crate change rather than a handler fix. Suites across the workspace
depend on the present allowance to attach an AWS-namespace ARN the catalog does not carry — among
them `services/service-com-amazonaws-ec2-amazonec2/tests/all/deny_over_an_unreadable_grant.rs`
(whose whole premise is a grant whose document the emulator cannot read, which is also the
`PrincipalPolicies::uncarried` branch of the IAM authorization gate),
`services/service-com-amazonaws-iam-awsidentitymanagementv20100508/tests/all/role_attached_policies.rs`
and `.../user_attached_policies.rs` (which fill the per-entity quota with invented
`QuotaFiller`/`UserQuotaFiller` ARNs), `.../user_permissions_boundary.rs` and
`.../role_permissions_boundary.rs` (reserved-path refusals on uncarried names),
`services/service-com-amazonaws-apigateway-backplanecontrolservice/tests/all/control_plane_authorization.rs`,
`services/service-com-amazonaws-evs-amazonelasticvmwareservice/tests/all/identity_policy_sources.rs`,
`services/service-com-amazonaws-b2bi-b2bi/tests/all/authorization.rs`,
`services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/tests/all/{runtime,unnameable}_identifier_authorization.rs`
and `services/service-com-amazonaws-transfer-transferservice/tests/all/identity_and_authorization.rs`.
The fix is therefore: grow the catalog to carry every AWS-managed policy the emulator's own services
and suites name, make `is_published_aws_managed_policy_arn` authoritative (`None => false`), and
re-state the suites above on a grant the catalog carries — one change, taken as its own issue so it
is reviewed as the model change it is.
Traces: iam.op-contract-attach-user-policy.

## Evidence
Replay of this issue's units after the parent's cause 3 was fixed:
`iam.get-aws-managed-policy-metadata-and-default-version` PASSES (`iam|GetPolicy|NoSuchEntity`
reported `fixed`), and the four units below still fail, each on the key recorded above, with
`other_causes` empty — so the parent fix caused none of them.

  [mismatch] iam.iam-access-key-crud
    AssertionError: expected AKIA-prefixed access key id, got 'EMULATORAKFIFW45YVTY'
  [mismatch] iam.user-access-key-round-trips-status-and-never-used
    AssertionError: a long-term key id starts AKIA: 'EMULATORAKIZZKSHNMYQ'
  (the ids above are per-run; the requirement is the `AKIA`+16 form)

  [mismatch] iam.requested-region-for-global-iam-is-us-east-1-other-region-denied
    AssertionError: iam:ListRoles: AccessDenied message: expected 'User:
    arn:aws:iam::<account>:user/<user> is not authorized to perform: iam:ListRoles on resource:
    arn:aws:iam::<account>:role/ because no identity-based policy allows the iam:ListRoles action',
    got the same sentence plus the `. Go to https://...` authorization-details trailer. The
    `iam:ListAccountAliases` leg of the same check requires that trailer and passes.

  [mismatch] iam.op-contract-attach-user-policy
    AssertionError: expected a rejection, but the call SUCCEEDED and returned {'ResponseMetadata':
    {... 'HTTPStatusCode': 200 ...}} — on
    attach_user_policy(UserName=<user>, PolicyArn='arn:aws:iam::aws:policy/sim2real-does-not-exist-policy').
    Expected HTTP 404 / NoSuchEntity / 'Policy <arn> does not exist or is not attachable.'

## Steps to Reproduce
1. Cause 1: CreateUser, then CreateAccessKey and inspect `AccessKey.AccessKeyId` — 20 characters
   beginning `AKIA` are required; `ListAccessKeys` must project the same form. Fixing it inside a
   service crate is what `vendor/server/src/aws_credentials.rs:47` prevents.
2. Cause 2: CreateUser + CreateAccessKey, PutUserPolicy with `Allow` statements conditioned on
   `aws:RequestedRegion` ({StringEquals: eu-west-1} on iam:ListAccountAliases, {StringNotEquals:
   us-east-1} on iam:ListRoles), then call both as that user: the ListAccountAliases denial must
   carry the trailer and the ListRoles denial must not.
3. Cause 3: CreateUser, then AttachUserPolicy with
   `arn:aws:iam::aws:policy/sim2real-does-not-exist-policy` — expect 404 NoSuchEntity and nothing
   attached; observed 200. Check `AttachRolePolicy` / `AttachGroupPolicy` and the
   permissions-boundary puts, which share the resolution.
