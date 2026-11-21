Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.cf-cache-behaviors-share-one-trusted-key-group cloudfront.distribution-many-cache-behaviors-order-and-count-preserved cloudfront.distribution-cache-behavior-count-quota-boundary-rejected cloudfront.list-field-level-encryption-profiles-pagination-walks-full-set cloudfront.delete-key-group-trusted-by-distribution-rejected cloudfront.invalidation-cache-tag-paths-accepted-and-read-back cloudfront.invalidation-for-distribution-tenant-quantity-validated-before-tenant-lookup
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreateDistribution|TooManyKeyGroupsAssociatedToDistribution
Issue-Key: cloudfront|CreateDistribution|TooManyCacheBehaviors
Issue-Key: cloudfront|ListFieldLevelEncryptionProfiles|InvalidArgument
Issue-Key: cloudfront|ListDistributionsByKeyGroup|InvalidArgument
Issue-Key: cloudfront|CreateInvalidation|InvalidArgument
Issue-Key: cloudfront|CreateInvalidationForDistributionTenant|EntityNotFound
Issue-Related: 20261117T151821Z-cloudfront-6-net-new-refusal-wording-causes-emulator.md
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudfront: 5 net-new false-refusal causes — key groups and cache behaviors counted per reference, list Markers rejected, cache-tag invalidation paths refused, tenant invalidation validated out of order

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Five net-new root causes where the emulator refuses a call AWS accepts, or refuses it for the wrong reason. These are the mirror image of the missing-validation issue: here legitimate requests are blocked, so the affected checks cannot even reach their assertions. All five must be closed before submitting.

Cause 1 of 5: trusted key groups are counted per cache-behavior reference instead of per distinct key group.
AWS: Smithy: TrustedKeyGroups is a per-cache-behavior structure (Enabled/Quantity/Items) and CreateDistribution binds TooManyKeyGroupsAssociatedToDistribution (400) only when the number of key groups associated to the distribution exceeds the quota. Recorded behavior: a distribution whose default behavior and four additional behaviors all name the SAME key group is created, and GetDistribution reports ActiveTrustedKeyGroups.Quantity 1 — the accounting is over distinct key groups, not over references.
Emulator: refuses CreateDistribution with 400 `TooManyKeyGroupsAssociatedToDistribution` ("This distribution references 5 key groups; at most 4 are allowed per distribution") for five behaviors that share one key group.
Why: the normal signed-URL pattern — one key group trusted by every behavior — is impossible to provision, and a client sees a quota error for a distribution that uses a single key group.
Fix: CreateDistribution/UpdateDistribution key-group quota accounting — count the distinct key group ids across all behaviors, and report ActiveTrustedKeyGroups as the distinct set.
Traces: cloudfront.cf-cache-behaviors-share-one-trusted-key-group.

Cause 2 of 5: the cache-behavior quota is enforced at the wrong number (and counts the default behavior).
AWS: Smithy: CreateDistribution binds TooManyCacheBehaviors (400). Recorded behavior: a config with 26 distinct ordered CacheBehaviors on one origin is CREATED (GetDistributionConfig reports CacheBehaviors.Quantity 26 in submitted order), and a config at 74 behaviors is created while 75 is refused — so AWS's ceiling is well above 25 and the DefaultCacheBehavior is not charged against CacheBehaviors.
Emulator: refuses at 26 with `TooManyCacheBehaviors` "This distribution states 26 cache behaviors; at most 25 are allowed per distribution" — the 26-behavior positive case never gets past create, and the 74-behavior boundary case is refused too.
Why: distributions AWS accepts cannot be created at all; ordering/read-back behavior of a realistic multi-behavior distribution is unobservable.
Fix: CreateDistribution's cache-behavior quota check (the limit value, and excluding DefaultCacheBehavior from the count).
Traces: cloudfront.distribution-many-cache-behaviors-order-and-count-preserved, cloudfront.distribution-cache-behavior-count-quota-boundary-rejected.

Cause 3 of 5: list operations reject their own Marker (and an absent Marker).
AWS: Smithy: ListFieldLevelEncryptionProfiles and ListDistributionsByKeyGroup take Marker as an optional httpQuery string; a Marker echoed from the previous page's NextMarker is valid, and omitting Marker means "first page". Recorded behavior: paging with the returned marker walks the full set; a call with no Marker returns the first page.
Emulator: answers 400 `InvalidArgument` "Marker must be a valid CF field-level encryption profile Id" when paging with the marker it just returned, and 400 `InvalidArgument` "Marker  is not a distribution id returned by ListDistributionsByKeyGroup" (note the empty value) when no Marker is supplied.
Why: pagination is unusable — a client cannot walk a list, and a plain unpaginated list call fails outright; the key-group deletion check cannot even enumerate the distributions that reference the group.
Fix: the shared CloudFront list-Marker validation — accept an absent Marker and accept a marker the service itself minted.
Traces: cloudfront.list-field-level-encryption-profiles-pagination-walks-full-set, cloudfront.delete-key-group-trusted-by-distribution-rejected.

Cause 4 of 5: CreateInvalidation refuses cache-tag invalidation paths.
AWS: Smithy: CreateInvalidation takes an InvalidationBatch whose Paths is a list of strings and binds InvalidArgument (400); CacheTagConfig's documentation states cached objects can be invalidated by tag through CreateInvalidation. Recorded behavior: on a distribution with CacheTagConfig set, a Paths item beginning with '#' (a cache tag, e.g. '#product:123') is accepted alone and mixed with an ordinary '/path' item, and GetInvalidation reports exactly the submitted paths.
Emulator: answers 400 `InvalidArgument` "Your request contains one or more invalid invalidation paths." for the cache-tag path.
Why: tag-based invalidation — the whole point of CacheTagConfig — cannot be requested at all.
Fix: CreateInvalidation's Paths validation (allow cache-tag paths when the distribution carries CacheTagConfig) and the invalidation state that echoes them back.
Traces: cloudfront.invalidation-cache-tag-paths-accepted-and-read-back.

Cause 5 of 5: a tenant invalidation resolves the tenant before validating the batch's quantity.
AWS: Smithy: CreateInvalidation binds InconsistentQuantities (400) and EntityNotFound/NoSuchDistribution (404). Recorded behavior for a batch whose Paths.Quantity disagrees with the supplied items AND whose tenant does not exist: 400 `InconsistentQuantities`, Message "The specified quantity of Paths (1) does not match the actual quantity supplied (2)." — the fixed text is "The specified quantity of Paths (" ... ") does not match the actual quantity supplied (" ... ")." with the declared and actual counts from the request; the quantity check runs before the tenant lookup.
Emulator: answers `EntityNotFound` "The specified distribution tenant doesn't exist." — the lookup runs first.
Why: a client with a malformed batch is told the tenant is missing and goes looking for a provisioning bug instead of fixing its own Quantity field.
Fix: ordering in CreateInvalidation's validation — validate the InvalidationBatch quantities before resolving the tenant/distribution.
Traces: cloudfront.invalidation-for-distribution-tenant-quantity-validated-before-tenant-lookup.

## Evidence
Behavioral suite simparity-behavioral-aws-v1; all checks below are listed under `### cloudfront`, neither ALREADY FILED nor OWNED.

Cause 1 — cf-cache-behaviors-share-one-trusted-key-group [error]: botocore.errorfactory.TooManyKeyGroupsAssociatedToDistribution on CreateDistribution — "This distribution references 5 key groups; at most 4 are allowed per distribution"; the check creates ONE key group and names it from five behaviors, and asserts ActiveTrustedKeyGroups.Quantity == 1.
Cause 2 — distribution-many-cache-behaviors-order-and-count-preserved [error]: botocore.errorfactory.TooManyCacheBehaviors on CreateDistribution — "This distribution states 26 cache behaviors; at most 25 are allowed per distribution" (the check submits 26 and expects 201); distribution-cache-behavior-count-quota-boundary-rejected submits 75 (limit 74) and the emulator reports the 25 ceiling.
Cause 3 — list-field-level-encryption-profiles-pagination-walks-full-set [error]: botocore.errorfactory.InvalidArgument on ListFieldLevelEncryptionProfiles — "Marker must be a valid CF field-level encryption profile Id"; delete-key-group-trusted-by-distribution-rejected [error]: InvalidArgument on ListDistributionsByKeyGroup — "Marker  is not a distribution id returned by ListDistributionsByKeyGroup".
Cause 4 — invalidation-cache-tag-paths-accepted-and-read-back [error]: botocore.errorfactory.InvalidArgument on CreateInvalidation — "Your request contains one or more invalid invalidation paths." for Paths ['#product:123'].
Cause 5 — invalidation-for-distribution-tenant-quantity-validated-before-tenant-lookup: "expected ('InconsistentQuantities', 'The specified quantity of Paths (1) does not match the actual quantity supplied (2).'), got ('EntityNotFound', \"The specified distribution tenant doesn't exist.\")".

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator.
2. Replay the simparity-behavioral-aws-v1 suite.
3. Re-run the checks in trace_ids.
4. Cause 1: CreateDistribution with one key group named by the default behavior and four more behaviors — expect 201 and ActiveTrustedKeyGroups.Quantity 1. Cause 2: CreateDistribution with 26, then 74, then 75 CacheBehaviors — expect 201, 201, TooManyCacheBehaviors. Cause 3: ListFieldLevelEncryptionProfiles with MaxItems smaller than the set and then the returned NextMarker; ListDistributionsByKeyGroup with no Marker. Cause 4: CreateInvalidation with Paths ['#product:123'] and ['#category:electronics','/index.html'] on a distribution with CacheTagConfig, then GetInvalidation. Cause 5: CreateInvalidation for a nonexistent tenant with Paths.Quantity 1 and two items — expect InconsistentQuantities first.
