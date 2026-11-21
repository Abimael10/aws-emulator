Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-distribution-tenant-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-distribution-tenant-lifecycle
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|ListDistributionTenants|2xx->2xx|$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag
Issue-Commit: 597db9eebaefdf1df0a6fca4ca797f56db8217fd

# cloudfront: ListDistributionTenants summary reports an ETag that is not the tenant's current version

> _Note: this issue was identified against the code base at commit `597db9eebaefdf1df0a6fca4ca797f56db8217fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDistributionTenants

## Description
One cause.

Cause 1 of 1: ListDistributionTenants returns a DistributionTenantSummary whose ETag is not the distribution tenant's current version token (fixture cloudfront-distribution-tenant-lifecycle, step task#4).

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json) models ListDistributionTenants -> ListDistributionTenantsResult.DistributionTenantList of DistributionTenantSummary, whose member ETag is @required and documented as "The current version of the distribution tenant." That is the SAME value the resource's own version token carries everywhere else on this surface: CreateDistributionTenant, GetDistributionTenant and UpdateDistributionTenant all bind an output member ETag (httpHeader: ETag) documented identically, and UpdateDistributionTenant/DeleteDistributionTenant take that token back as the @required If-Match header (modeled errors InvalidIfMatchVersion 400 and PreconditionFailed 412). So for a tenant that has not been mutated since the last mutation that handed a token out, the ETag a listing reports and the ETag a get/create/update of that same tenant reports are one and the same string; a listing never mints or reports a different token. The version token's concrete text is a generated value and is not itself a requirement — the requirement is that the listing's token EQUALS the tenant's current one, which is what the recorded step asserts (the expected value is the token bound earlier in the same trace).

Emulator: the replayed listing reports a DIFFERENT token for the same tenant in the same trace — $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag: value_mismatch (expected 'EJZ50Z92T9DP30', got 'E1MJV490Q2CKJ1'), HTTP 200 both sides, every other member of the summary matching. The two strings are both this emulator's own ETag shape, so the summary is sourcing some token other than the record's: GetDistributionTenant answers from the stored record (src/ops/get_distribution_tenant.rs:156, e_tag: Some(stored.etag.clone())), while the listing projects each record through store.distribution_tenant_summary(stored) (src/ops/list_distribution_tenants.rs:287), and that projection does not carry StoredDistributionTenant::etag (src/state.rs:2083-2085, "the version token the last mutation handed back, and the single source of truth for every If-Match check against this tenant") into the summary's ETag member.

Why: a client that enumerates tenants and then conditionally mutates one — the documented flow, since the summary carries the version precisely so a caller does not have to re-GET each tenant — sends the listed ETag as If-Match and is refused with InvalidIfMatchVersion/PreconditionFailed against AWS-correct client code, or (worse, when the emulator's own check is lenient) succeeds against a token that never identified that tenant's configuration, so the optimistic-concurrency guarantee the ETag exists to provide is silently void. Readers that cache the listing's version to detect drift also see spurious changes on every poll.

Fix: the ListDistributionTenants response projection — the DistributionTenantSummary builder (store.distribution_tenant_summary, called from the handler) must read the tenant record's stored version token for the summary's ETag member, the same field GetDistributionTenant/CreateDistributionTenant/UpdateDistributionTenant hand back and the same field the If-Match checks compare against, instead of minting or borrowing another token. No new state is needed: the record already holds it.

Traces: cloudfront-distribution-tenant-lifecycle#task#4 (ListDistributionTenants, 200 vs 200, ETag value mismatch). No currently open issue carries this replay key: the related cloudfront issues 20261105T231732Z-cloudfront-readback-omits-defaulted-members-and-reports-stale.md, 20261114T142904Z-cloudfront-reopens-generated-identifiers-and-published-domain-names.md and 20261029T212223Z-cloudfront-if-match-etag-handling-wrong-precondition-message.md are about defaulted/stale readback members, generated identifiers and the precondition message wording respectively, none about a listing summary's ETag disagreeing with the resource's current token.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture cloudfront-distribution-tenant-lifecycle, step task#4, operation ListDistributionTenants, verdict mismatch.

Expected: HTTP 200 with $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag equal to the tenant's current version token, i.e. the same string the preceding create/update of that tenant returned in its ETag output (recorded expected value 'EJZ50Z92T9DP30' — that literal is a per-run generated token, the requirement is the equality, not the text).

Actual: HTTP 200 with that ETag = 'E1MJV490Q2CKJ1', a different token from the one the same tenant's mutation/get reports in the same trace. All other asserted members of the summary matched.

Code: src/ops/list_distribution_tenants.rs:287 projects each stored tenant with store.distribution_tenant_summary(stored); src/ops/get_distribution_tenant.rs:156 answers with stored.etag; src/state.rs:2083-2085 declares StoredDistributionTenant::etag as the single source of truth for this tenant's version.

## Steps to Reproduce
1. Replay fixture cloudfront-distribution-tenant-lifecycle against the emulator (replay_wire_corpus) and look at step task#4.
2. Equivalently, by hand: POST /2020-05-31/distribution-tenant to create a tenant under a multi-tenant distribution and note the ETag returned (header/output member).
3. GET /2020-05-31/distribution-tenant/{Id} and confirm the same ETag is returned.
4. POST /2020-05-31/distribution-tenants (ListDistributionTenants) with no AssociationFilter and read the DistributionTenantSummary for that tenant id.
5. Observe the summary's ETag differs from the token steps 2 and 3 returned; AWS reports the identical token. Using the listed token as If-Match on UpdateDistributionTenant/DeleteDistributionTenant therefore does not refer to the tenant's current version.

## Resolution

Cause 1 DECLINED as a per-run value, and the rule behind it pinned by a suite.

- The equality the cause is about already holds in this tree: a distribution tenant's version
  token is minted by the call that CHANGES the tenant (`state/store.rs`'s
  `apply_distribution_tenant_update` and `apply_tenant_web_acl`, and the create), stored on the
  record once, and read back from that one field by every projection that carries an `ETag` —
  `StoredDistributionTenant::to_summary` (`state.rs`) reads `self.etag` for the summary BOTH
  listings report through `distribution_tenant_summary`, and `GetDistributionTenant`,
  `GetDistributionTenantByDomain`, the create, the update and the web ACL association report the
  same field. Nothing on a read path mints or moves one, the clock-driven `InProgress` ->
  `Deployed` transition included.
- What remains is the TEXT of the token, which is a value the service generates per run and no
  rule derives from the request: the recorded expectation is byte-identical across sweeps
  (`EJZ50Z92T9DP30` in 20261109, 20261119 and this report) while this emulator's token changed
  with unrelated changes to its mint (`E89FKDDMX3R403`, then `E1MJV490Q2CKJ1`), so the step
  compares the recording's literal rather than the token handed out earlier in the same replayed
  run. Answering it would mean fitting the recording.
- `tests/all/tenant_version_token_is_minted_by_a_write.rs` pins the half the CLOCK owns — every
  reader (the tenant's own read, an unfiltered listing, a listing filtered by the distribution and
  a walk of one-entry pages) reports the token the last write minted, asked twice over while the
  tenant propagates and again once it is deployed, and the token a deployed tenant's listing hands
  out is still the only one its `If-Match` accepts. `tests/all/tenant_listing_version_token.rs`
  pins the half a WRITE owns.
