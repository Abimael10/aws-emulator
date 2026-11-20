Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-key-group-config-bounds-and-member-dedup
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-key-group-config-bounds-and-member-dedup cloudfront-distribution-tenant-lifecycle
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|UpdateKeyGroup|2xx->2xx|$.KeyGroup.KeyGroupConfig.Items.PublicKey
Issue-Key: cloudfront|ListDistributionTenants|2xx->2xx|$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag
Issue-Commit: a0a325cd7a8f100e0b15b3c52772a25e70aa00e8

# CloudFront key-group and distribution-tenant readbacks publish stored state AWS does not

> _Note: this issue was identified against the code base at commit `a0a325cd7a8f100e0b15b3c52772a25e70aa00e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateKeyGroup

## Description
TWO (2) distinct net-new causes, one shared fault: a 2xx response projects the emulator's stored state for a resource, and that projection is not the state AWS publishes — a repeated list member AWS collapses is serialized twice, and a resource-version (ETag) member is not the resource's current version. Both must be closed before this issue is submitted. Replay reports both clusters unowned (stub_path="", owned_by=[]).

=== Cause 1 of 2: UpdateKeyGroup's 200 response repeats a duplicated public key id instead of collapsing it ===
AWS: recorded behavior only for the de-duplication — Smithy places no @uniqueItems on PublicKeyIdList, so the collapse is not modeled. Smithy: UpdateKeyGroup http {method PUT, uri /2020-05-31/key-group/{Id}, code 200}; KeyGroupConfig is @required and the httpPayload on input, KeyGroup is the httpPayload on output with an ETag response header. The recording shows that when the submitted KeyGroupConfig.Items names the same public key id twice, the 200 response's KeyGroup.KeyGroupConfig.Items carries that id exactly ONCE — a single PublicKey element.
Emulator: 200, but $.KeyGroup.KeyGroupConfig.Items.PublicKey is a two-element list repeating the same id (replay verdict type_mismatch: a list where AWS serializes one scalar element).
Why: the response's SHAPE changes — a client deserializing Items gets two entries where AWS gives one, an XML-shape-sensitive parser sees a repeated element where a single one is expected, and any member count / key-group quota arithmetic the client does over Items is off. Because the duplicate is persisted, a read-modify-write round trip re-submits it and the divergence compounds.
Fix: UpdateKeyGroup's state write for the key group's public-key id list (collapse repeated ids on store), with the UpdateKeyGroup serializer following the stored list.
Related-but-not-owning: the open issue 20261118T074023Z-cloudfront-key-group-bounds-member-dedup-and-etag.md files the same de-duplication fault on CreateKeyGroup and GetKeyGroupConfig and does NOT carry this replay key (cloudfront|UpdateKeyGroup|2xx->2xx|$.KeyGroup.KeyGroupConfig.Items.PublicKey) — that issue records this step answering 412 PreconditionFailed, so UpdateKeyGroup's own body was never compared; it now answers 200 and the duplicated Items are exposed on this third, separately-served response.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#task#11.

=== Cause 2 of 2: ListDistributionTenants publishes a summary ETag that is not the tenant's current version ===
AWS: Smithy: ListDistributionTenants http {method POST, uri /2020-05-31/distribution-tenants, code 200}, output DistributionTenantList of DistributionTenantSummary; DistributionTenantSummary.ETag is @required with doc "The current version of the distribution tenant." So the listed ETag is required to be the version of the tenant as the emulator currently holds it — the same opaque version its create/update published and the value a client may hand back as If-Match.
Emulator: 200 with $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag carrying a value that is not the one the recording reports for that tenant at this point of its lifecycle (value_mismatch). Hypothesis: the list projection mints or re-derives an ETag of its own instead of reading the tenant's stored version, so the listing's version does not track the one the mutation path published.
Why: ETag is the concurrency token for the tenant — a client that discovers a tenant by listing and then conditionally updates or deletes it with the ETag it just read is either refused with a spurious PreconditionFailed or, worse, allowed to act on a version it never observed, defeating optimistic-concurrency control over distribution tenants.
Fix: ListDistributionTenants' DistributionTenantSummary projection and the distribution-tenant ETag state in cloudfront state — publish the tenant's stored current version in the summary rather than computing a list-local value.
Note: the two literal ETag strings are per-run opaque values and neither is the requirement; the requirement is that the summary's ETag EQUALS the tenant's current version, which is what the lifecycle fixture exercises across its steps.
Traces: cloudfront-distribution-tenant-lifecycle#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator_up=true, /__admin/health 200; 1411 steps, 1283 passed, score 90.9%). Two unowned mismatch clusters:

Cause 1 — UpdateKeyGroup 200->200, key $.KeyGroup.KeyGroupConfig.Items.PublicKey (cloudfront-key-group-config-bounds-and-member-dedup#task#11, 1 affected step), divergence_key 'service=cloudfront|UpdateKeyGroup|status=200->200|keys=$.KeyGroup.KeyGroupConfig.Items.PublicKey': expected the single scalar id 'K7IYCGPKTANSF3'; actual the two-element list ['K7IYCGPKTANSF3', 'K7IYCGPKTANSF3'] (type_mismatch). stub_path="", owned_by=[].

Cause 2 — ListDistributionTenants 200->200, key $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag (cloudfront-distribution-tenant-lifecycle#task#4, 1 affected step), divergence_key 'service=cloudfront|ListDistributionTenants|status=200->200|keys=$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag': expected 'EJZ50Z92T9DP30', actual 'E1MJV490Q2CKJ1' (value_mismatch). Both strings have the same E+13-alphanumeric form, so the form is satisfied and the divergence is in which version the summary reports; the literal values are per-run. stub_path="", owned_by=[].

## Steps to Reproduce
1. git clone the repo, boot the emulator built from that checkout, and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Representative step: fixture cloudfront-key-group-config-bounds-and-member-dedup, step task#11 — PUT /2020-05-31/key-group/<Id> whose KeyGroupConfig.Items names the same public key id twice, with the current If-Match. Inspect KeyGroup.KeyGroupConfig.Items in the 200 body: AWS serializes one PublicKey element, the emulator serializes two.
3. Fixture cloudfront-distribution-tenant-lifecycle, step task#4 — POST /2020-05-31/distribution-tenants and read DistributionTenantList/DistributionTenantSummary/ETag. Compare it with the ETag the tenant's preceding create/update step in the same fixture published, and then re-use the listed ETag as If-Match on an update of that tenant: it must be accepted as the tenant's current version.

## Resolution

Cause 1 FIXED, cause 2 DECLINED as a per-run value (and pinned by a suite instead).

- Cause 1 FIXED: a key group's `Items` is the group's MEMBERSHIP, so a public key id a payload
  repeats names one member. The submitted configuration is now RESOLVED before it is judged and
  stored (`state::key_group_config::resolve_key_group_config`, reached by `CreateKeyGroup`,
  `UpdateKeyGroup` and `apply_key_group_config`), collapsing repeated ids and keeping the place of
  each id's first mention, so every readback of a group — the create's and the update's own body,
  `GetKeyGroup`, `GetKeyGroupConfig`, `ListKeyGroups` and a distribution's
  `ActiveTrustedKeyGroups` — reports each of its keys once, and a read-modify-write round trip is
  stable. `tests/all/key_group_items_are_a_set.rs` pins it over a table of repetition shapes on
  both writes and every readback.
- Along the way, so the replayed fixture gets past the causes it then failed on (all of them the
  property of a key group CONFIGURATION, judged in the one place both writes judge one), the
  refusals of that configuration now carry CloudFront's own wording and the two rules about its
  `Name`: a name past the length the service admits ("too big"), a name carrying a character it
  does not admit ("contains illegal characters"), a key list naming no public key (an invalid
  `publicKeyIdsList`), and a `Comment` past its member's documented bound ("too big", one sentence
  for every surface of this service that carries a `Comment`).
  `tests/all/key_group_configuration_members.rs` pins each sentence as a literal, at and past each
  bound, on the create and the update.
- Cause 2 DECLINED: a version token is a per-run, server-generated value. The summary a listing
  reports already carries the tenant's own stored current version — `state.rs`'s
  `StoredDistributionTenant::to_summary` reads `self.etag`, the token the create, the update, the
  web ACL association and `GetDistributionTenant` publish — so a client can take it out of a
  listing and modify or delete the tenant with it; nothing on a read path mints or re-derives one.
  `tests/all/tenant_listing_version_token.rs` pins that across the create, an
  `UpdateDistributionTenant` and an `AssociateDistributionTenantWebACL`, including the
  list-then-modify path and the refusal of the superseded token.
