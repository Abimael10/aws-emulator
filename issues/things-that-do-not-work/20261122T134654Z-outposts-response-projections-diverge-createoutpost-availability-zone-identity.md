Issue-Source: trace-sweep-divergence
Issue-Trace: outposts-site-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: outposts-site-lifecycle outposts-assets-and-connection outposts-capacity-task-lifecycle outposts-quote-and-order-lifecycle outposts-renewal-and-billing
Issue-Shape-Id: com.amazonaws.outposts#OutpostsOlafService
Issue-Key: outposts|UpdateSite|2xx->2xx|$.Site.Tags.resourceArn
Issue-Key: outposts|CreateOutpost|2xx->2xx|$.Outpost.AvailabilityZoneId
Issue-Commit: 49eb1163589ae97c687491b6fcd72c402bbeb921

# outposts response projections diverge: CreateOutpost Availability Zone identity and UpdateSite tags

> _Note: this issue was identified against the code base at commit `49eb1163589ae97c687491b6fcd72c402bbeb921`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateSite

## Description
2 net-new root causes, both in how this service PROJECTS a stored resource into a 200 response: the Site a successful UpdateSite answers loses the site's tag map, and the Outpost a successful CreateOutpost answers carries a different AvailabilityZoneId than AWS reports for the same request. Neither is covered by the open issues 20261029T191039Z-outposts-lifecycle-and-precondition-gates-diverge-across-capacity (status/precondition gates and renewal pricing) or 20261121T145507Z-outposts-listcatalogitems-publishes-an-invented-hardware-catalog-instead (the published catalog); both of those are about other operations and other members.

Cause 1 of 2: UpdateSite answers a Site whose Tags member has lost the tags the site carries.
AWS: Smithy UpdateSite (PATCH /sites/{SiteId}) -> UpdateSiteOutput{Site: Site}; Site$Tags (TagMap, doc "The site tags."). A site's tags are part of the site, so every projection of it — including the one a partial update answers — reports the site's whole tag map; UpdateSite takes only SiteId/Name/Description/Notes and therefore cannot change tags, so an update leaves them exactly as they were. The recorded response is 200 and its Site.Tags contains the entry the site was tagged with before the update: key as supplied by the earlier tagging call (recorded key `resourceArn`), value as supplied (recorded value the site's own ARN, i.e. the template arn:aws:<partition>:outposts:<region>:<accountId>:site/<siteId> filled from the endpoint and store — the literal ARN of one recording is a generated value, the presence of the key with the value as supplied is the requirement).
Emulator: 200 with the same Site, but the tag entry is absent from $.Site.Tags — the replay reports `$.Site.Tags.resourceArn: missing`. The tag the caller applied to that site is not reported by the update at all.
Why: a client that renames or annotates a site and then reads the returned Site back sees its tags vanish, so tag-driven bookkeeping (cost allocation, inventory selection, "tag then update" provisioning scripts) silently loses state, and a round-trip through UpdateSite looks like an untag the caller never requested.
Fix: the UpdateSite handler's Site projection and the site tag state it reads — services/service-com-amazonaws-outposts-outpostsolafservice/src/ops/update_site.rs (the handler answers record.site.clone() after applying name/description/notes) together with the site arm of the store's tag map (src/state/store.rs, tags/tags_mut keyed by SITE_RESOURCE_TYPE, which TagResource in src/ops/tag_resource.rs writes through). Hypothesis: the tag applied to the site is not on the stored Site record the update projects by the time UpdateSite resolves it (the tagging write and the update's lookup do not land on the same stored record), so the update reports a tagless Site. The requirement either way: the Site answered by UpdateSite reports every tag the site holds, as a present map (empty when it holds none), and an update never drops one.
Traces: outposts-site-lifecycle#task#4.

Cause 2 of 2: CreateOutpost resolves the AvailabilityZoneId from the AvailabilityZone name with a bijection that disagrees with the recorded one.
AWS: Smithy CreateOutpost (POST /outposts) accepts AvailabilityZone (pattern ^([a-zA-Z]+-){1,3}([a-zA-Z]+)?(\d+[a-zA-Z]?)?$) OR AvailabilityZoneId (pattern ^[a-zA-Z]+\d-[a-zA-Z]+\d$) and answers Outpost with BOTH Outpost$AvailabilityZone and Outpost$AvailabilityZoneId populated; the model and the AWS docs publish no formula pairing a zone NAME with a zone ID (zone IDs are stable, zone names are per-account), so this is recorded behavior only. For the zone name these fixtures create their Outpost in, the recorded 200 response reports AvailabilityZoneId 'usw2-az2', consistently in all four recordings.
Emulator: 200 with the same AvailabilityZone but AvailabilityZoneId 'usw2-az1' — the replay reports `$.Outpost.AvailabilityZoneId: value_mismatch (expected 'usw2-az2', got 'usw2-az1')`. The handler derives the id the request did not name from the name it did (src/ops/create_outpost.rs) through a fixed letter<->index bijection (state.rs zone_index/availability_zone_id: first zone letter <-> -az1), which is one index off the pairing the recordings show, so every Outpost created by naming a zone name reports the wrong zone id, and every later read of that Outpost (GetOutpost/ListOutposts) repeats it.
Why: a client that creates an Outpost by zone NAME and then selects or asserts on the returned zone ID — the identifier AWS recommends for cross-account zone matching — gets a zone id that names a different physical zone than AWS returned, so placement and zone-pinning logic keyed on AvailabilityZoneId diverges from AWS.
Fix: the zone-identity resolution shared by CreateOutpost — services/service-com-amazonaws-outposts-outpostsolafservice/src/state.rs (availability_zone_id / availability_zone / zone_index / zone_letter) — so the name<->id pairing this service publishes matches the recorded one in both directions (and the pair-agreement check in availability_zone_refusal stays consistent with it).
Traces: outposts-assets-and-connection#setup#2, and the same step of outposts-capacity-task-lifecycle, outposts-quote-and-order-lifecycle and outposts-renewal-and-billing.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (89 steps, 78 passed, 11 mismatched; these are the 2 clusters not already owned by an open issue or a stub):
- UpdateSite (outposts-site-lifecycle#task#4): status 200 expected and got 200, but '$.Site.Tags.resourceArn' missing — expected 'arn:aws:outposts:us-west-2:000000000000:site/os-b2ef7ab7eb7b2621e' (the site's own ARN, as the tag value supplied earlier in the fixture), got None. The whole tag entry is absent from the Site the update answers.
- CreateOutpost (outposts-assets-and-connection#setup#2, cluster of 4): status 200 expected and got 200, but '$.Outpost.AvailabilityZoneId' value_mismatch — expected 'usw2-az2', got 'usw2-az1'. $.Outpost.AvailabilityZone matched, so the diverging value is the id the emulator derived from the supplied zone name. Same divergence at setup#2 of outposts-capacity-task-lifecycle, outposts-quote-and-order-lifecycle and outposts-renewal-and-billing.
Model evidence: smithy_query(operation="UpdateSite") — input members SiteId/Name/Description/Notes only, output Site; shape Site member Tags (TagMap, "The site tags."). smithy_query(operation="CreateOutpost") — AvailabilityZone and AvailabilityZoneId both optional inputs, output Outpost carrying both members.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Cause 1 — representative fixture outposts-site-lifecycle, step task#4: CreateSite, apply a tag to the returned site ARN with TagResource (POST /tags/{ResourceArn}), then PATCH /sites/{SiteId} with a changed Name/Description/Notes. The emulator answers 200 with Site.Tags missing the tag that was applied; AWS answers 200 with Site.Tags containing it (key and value exactly as supplied).
3. Cause 2 — fixture outposts-assets-and-connection, step setup#2 (and setup#2 of outposts-capacity-task-lifecycle, outposts-quote-and-order-lifecycle, outposts-renewal-and-billing): CreateSite, then POST /outposts naming only AvailabilityZone (no AvailabilityZoneId). The emulator answers Outpost.AvailabilityZoneId 'usw2-az1'; the recording reports 'usw2-az2' for the same request. Re-read the Outpost with GetOutpost to see the stored value repeated.
