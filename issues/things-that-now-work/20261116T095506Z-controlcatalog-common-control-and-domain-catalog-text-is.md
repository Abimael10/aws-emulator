Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-common-controls
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-common-controls controlcatalog-catalog-list-domains
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/1nxqoydhsjdyboc4frsrrmev"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/2o50zyyv219ir9r7brjq0y37f"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/2o50zyyv219ir9r7brjq0y37f"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/8xy67kb7kojr38ozqpmrm60d7"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/bgzs5hpn3gsbmtvdzi7vuu7nb"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/bgzs5hpn3gsbmtvdzi7vuu7nb"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/ez1ccu4xkxrmlekcvjqo3jk59"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/ez4hcdf2fpirhuyojurkval9w"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/3t7xstyxx02pdwx9qas1h0quh"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/5m9jk2j28m8m4nr53u4c3br0h"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/5m9jk2j28m8m4nr53u4c3br0h"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/7encqm6cfsw704eoahh3ujr7y"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/dlukqke1ha1ean2txc1juehes"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/gr180qll01cux9k41amnf002"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/gr180qll01cux9k41amnf002"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/1tejgq26c0djpzgskw31uscm4"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/1tejgq26c0djpzgskw31uscm4"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/1ukpmkewk4i92tjmhsvewi4y7"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/5u2qgwuw3z1y0lrof60yf6264"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/5u2qgwuw3z1y0lrof60yf6264"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/c0qrxhefhmxkbq22tiejp3enn"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/c0qrxhefhmxkbq22tiejp3enn"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/d4s7ik8fgv8082v3x31hifzcc"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/ec1fxlvgtcxlf2nzremqcca7r"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/ec1fxlvgtcxlf2nzremqcca7r"].Name
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/eg1hxxu2e77a7w2wv79quwaxl"].Description
Issue-Key: controlcatalog|ListCommonControls|2xx->2xx|$.CommonControls[Arn="arn:aws:controlcatalog:::common-control/eg1hxxu2e77a7w2wv79quwaxl"].Name
Issue-Key: controlcatalog|ListDomains|2xx->2xx|$.Domains[Arn="arn:aws:controlcatalog:::domain/eyrstdrz8kmo28x5j8vgal6au"].Description
Issue-Related: 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md
Issue-Commit: 1490d5d01f48dd097a9435a06996bc23d586834e

# controlcatalog common-control and domain catalog text is emulator-authored, and ListDomains serves a different domain set (5 causes)

> _Note: this issue was identified against the code base at commit `1490d5d01f48dd097a9435a06996bc23d586834e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCommonControls

## Description
ListCommonControls and ListDomains answer 200 with the correct wire shape, but the taxonomy records they serve carry emulator-authored prose in place of the published catalog text, and ListDomains additionally serves a domain set that is not the catalogued one. Five divergence clusters share one root cause (the seeded common-control/domain taxonomy rows hold re-written Name/Description strings, and the domain table's membership and ARNs do not match the catalog), so they are bundled here as 5 causes.

SHARED STATEMENT OF THE FAULT
AWS: Smithy (controlcatalog): ListCommonControls -> POST /common-controls, 200, paginated over ListCommonControlsResponse$CommonControls : list<CommonControlSummary> (required); ListDomains -> POST /domains, 200, paginated over ListDomainsResponse$Domains : list<DomainSummary> (required). CommonControlSummary$Name/$Description and DomainSummary$Name/$Description are AWS-owned catalog content for a given ARN — the published string for that record, byte for byte — and the set of ARNs a listing returns is the published catalog's membership. None of these are caller-supplied or server-synthesizable values. The recording supplies the published strings and the published ARN set (recorded behavior is the authority for the per-record values; the model is the authority for these being AWS-owned catalog content, with Description documented as the record's console description).
Emulator: it serves short, differently worded sentences of its own for these records ('Release an approved change through an automated, repeatable deployment process, so that what was approved is what reaches the workload.' in place of the published paragraph, etc.), sometimes with a Name that names a different concept from the one the published record carries; and on ListDomains it serves Description rewrites for the catalogued domains plus whole DomainSummary entries at ARNs the recording does not contain while omitting catalogued ones.
Why: callers treat the ControlCatalog taxonomy as published AWS content: control-objective mapping, compliance-evidence export, console/portal rendering and catalog-diffing tools read Name/Description for an ARN and enumerate the domain list to drive navigation and reporting. A paraphrase changes what a common control is stated to require (one rewrite replaces 'Rollback and recovery procedures' with text about reviewing an emergency change after the fact — a different control entirely), and a domain list with ARNs AWS never published makes any client that pins or caches domain ARNs resolve records that do not exist in AWS.
Fix: the seeded taxonomy data behind the ListCommonControls and ListDomains handlers in the controlcatalog crate — the stored CommonControlSummary and DomainSummary rows must hold the published Name/Description strings verbatim and the domain table must hold exactly the catalogued domain ARNs, so handler and serializer pass them through unchanged. This is a state/data-fidelity defect, not a serialization defect.

Cause 1 of 5 — ListCommonControls, secure-development-lifecycle/change-management common controls (fixture controlcatalog-catalog-list-common-controls, step task#1). Four records diverge in one listing:
  common-control/1nxqoydhsjdyboc4frsrrmev Description — published text is the full 'Define, document, implement, and maintain a formal process for submitting and approving change requests. Change requests should outline the proposed changes, their impact, and any associated risks. The requests should then be reviewed and approved by the appropriate stakeholders, ensuring that changes are properly assessed, authorized, and tracked.'; the emulator truncates it to the first sentence.
  common-control/8xy67kb7kojr38ozqpmrm60d7 Description — published text describes an automated change deployment pipeline with automated security controls, validation checks and a rollback mechanism; the emulator serves a one-line paraphrase.
  common-control/2o50zyyv219ir9r7brjq0y37f Name and Description — published record is 'Rollback and recovery procedures' with the rollback/backup/recovery paragraph; the emulator serves a record about reviewing an emergency change after the fact.
  common-control/bgzs5hpn3gsbmtvdzi7vuu7nb Name and Description likewise re-written.
Trace: controlcatalog-catalog-list-common-controls#task#1.

Cause 2 of 5 — ListCommonControls, asset maintenance/monitoring common controls (step task#4).
  common-control/ez1ccu4xkxrmlekcvjqo3jk59 Description — published 'Monitor and optimize the performance and security of assets, including hardware and software.' vs emulator 'Monitor the condition and performance of assets, and act on what that monitoring reports before an asset fails.'
  common-control/ez4hcdf2fpirhuyojurkval9w Description — published 'Schedule and track maintenance activities for assets, including preventive maintenance and repairs.' vs emulator 'Maintain assets at the intervals their manufacturer or operator defines, and record the work carried out.'
Trace: controlcatalog-catalog-list-common-controls#task#4.

Cause 3 of 5 — ListCommonControls, asset classification common controls (step task#27). Seven keys diverge across five records: Descriptions of common-control/7encqm6cfsw704eoahh3ujr7y, /3t7xstyxx02pdwx9qas1h0quh, /dlukqke1ha1ean2txc1juehes are paraphrased, and /5m9jk2j28m8m4nr53u4c3br0h and /gr180qll01cux9k41amnf002 diverge in Name as well as Description — the published 'Asset classification and categorization' becomes 'Asset classification scheme', and the record published as asset labelling with unique identifiers is served as 'Review an asset's classification when the data it holds or the service it provides changes.'
Trace: controlcatalog-catalog-list-common-controls#task#27.

Cause 4 of 5 — ListCommonControls, asset inventory common controls (step task#35). Twelve keys diverge across six records (common-control/d4s7ik8fgv8082v3x31hifzcc, /1ukpmkewk4i92tjmhsvewi4y7, /c0qrxhefhmxkbq22tiejp3enn, /5u2qgwuw3z1y0lrof60yf6264, /1tejgq26c0djpzgskw31uscm4, /ec1fxlvgtcxlf2nzremqcca7r, /eg1hxxu2e77a7w2wv79quwaxl): Descriptions are paraphrased and several Names name a different control than the published one — the published 'Unauthorized asset management' record (remove/quarantine/deny connectivity for unauthorized assets) is served as 'Asset ownership assignment' with ownership text.
Trace: controlcatalog-catalog-list-common-controls#task#35.

Cause 5 of 5 — ListDomains serves a different domain taxonomy (fixture controlcatalog-catalog-list-domains, step task#1). Six catalogued domains come back with a re-written Description — e.g. domain/d4msesd9vvmzmmuvlv06m92uq published as the asset-management paragraph vs emulator 'Controls that inventory Amazon Web Services resources and keep that inventory current.'; domain/33mjpzadrlwo1by3c1012ai5i, /884eyhr622uy73h6i1mjb9lfb, /avh1ncrx6oyndrbsv0rfywvat, /eyrstdrz8kmo28x5j8vgal6au likewise. In the same response sixteen further domain ARNs differ wholesale (the entry is present on one side only): the recording contains catalogued ARNs such as domain/1yz9ep0ymkt1vufn0y3kyyhzv, /22ctir35fm1yojdh1sr4iucq7, /2oc9yao2szxsquya737481hw9, /70qzkmyi2s3oxheokg6jqqiy7, /996b3qixyfm57kat9vwwu8hy1, /acslzu9jr992jv4hjejegjnf0, /bn9ck1b74fp4b4wdysq0fanpc, /bqimrxnye01kph8z4lvy0ndp8, while the emulator's answer carries hex-shaped ARNs (domain/01d780d3ff2c8fd25185836de, /262d5c3b7a044409c5bb65351, /42a34489dabde1c06b0d71d19, /5a896e7e7ceb11a89e715befb, /d2e7eca3d54970a4b9a7706f8, /d6bd44b00c948ef00c6d8ddfa, /e37fac1dcbed9ed914ae39b0d, /ea07419970edbeb8b40f29027, /fc76959dae9dbcca8c896fd77). Hypothesis: those hex-shaped identifiers are minted locally rather than seeded from the catalog, which is why they do not appear in the recording.
Trace: controlcatalog-catalog-list-domains#task#1.

Relation to open issues: 20261106T092706Z-controlcatalog-taxonomy-list-operations-serve-none-of-the.md and 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md describe the same class of taxonomy defect, and 20261030T021409Z-controlcatalog-listcommoncontrols-returns-an-empty-commoncontrols-list-for-every--r2.md owns the separate 'empty CommonControls list' clusters in this same fixture; the replay reports no owner for the five clusters above, so they are net-new and filed here. A fixer should expect the correction to apply to the whole seeded common-control and domain taxonomy, not only the records named here.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixtures controlcatalog-catalog-list-common-controls and controlcatalog-catalog-list-domains: expected status 200, actual status 200, verdict mismatch on five clusters (all with empty owned_by and empty stub_path).

ListCommonControls task#1 — divergence key keys=$.CommonControls[Arn=".../1nxqoydhsjdyboc4frsrrmev"].Description, [.../2o50zyyv219ir9r7brjq0y37f].Description, [.../2o50zyyv219ir9r7brjq0y37f].Name, [.../8xy67kb7kojr38ozqpmrm60d7].Description, [.../bgzs5hpn3gsbmtvdzi7vuu7nb].Description, [.../bgzs5hpn3gsbmtvdzi7vuu7nb].Name. Samples:
  1nxqoydhsjdyboc4frsrrmev expected 'Define, document, implement, and maintain a formal process for submitting and approving change requests. Change requests should outline the proposed changes, their impact, and any associated risks. The requests should then be reviewed and approved by the appropriate stakeholders, ensuring that changes are properly assessed, authorized, and tracked.'; got 'Define, document, implement, and maintain a formal process for submitting and approving change requests.'
  8xy67kb7kojr38ozqpmrm60d7 expected 'Implement an automated change deployment pipeline that automates deployment of systems, software, and infrastructure as code (IaC) to production environments through a controlled and consistent process. Enforce automated security controls, perform validation checks, and provide a rollback mechanism in case of deployment failures.'; got 'Release an approved change through an automated, repeatable deployment process, so that what was approved is what reaches the workload.'
  2o50zyyv219ir9r7brjq0y37f expected Name 'Rollback and recovery procedures' and the rollback/backup/restore paragraph; got 'Review an emergency change after the fact, and record its approval.'

ListCommonControls task#4 — keys=$.CommonControls[Arn=".../ez1ccu4xkxrmlekcvjqo3jk59"].Description, [.../ez4hcdf2fpirhuyojurkval9w].Description.
  ez1ccu4xkxrmlekcvjqo3jk59 expected 'Monitor and optimize the performance and security of assets, including hardware and software.'; got 'Monitor the condition and performance of assets, and act on what that monitoring reports before an asset fails.'
  ez4hcdf2fpirhuyojurkval9w expected 'Schedule and track maintenance activities for assets, including preventive maintenance and repairs.'; got 'Maintain assets at the intervals their manufacturer or operator defines, and record the work carried out.'

ListCommonControls task#27 — 7 keys over records 7encqm6cfsw704eoahh3ujr7y, 3t7xstyxx02pdwx9qas1h0quh, dlukqke1ha1ean2txc1juehes, 5m9jk2j28m8m4nr53u4c3br0h (Description+Name), gr180qll01cux9k41amnf002 (Description+Name). Samples:
  7encqm6cfsw704eoahh3ujr7y expected 'Assign a value to assets based on their cost, replacement value, or other relevant factors.'; got 'Record what each asset is worth to the business, so that the protection it is given matches what its loss would cost.'
  5m9jk2j28m8m4nr53u4c3br0h expected Name 'Asset classification and categorization'; got 'Asset classification scheme'
  gr180qll01cux9k41amnf002 expected Description 'Label assets with unique identifiers.'; got "Review an asset's classification when the data it holds or the service it provides changes."

ListCommonControls task#35 — 12 keys over records 1tejgq26c0djpzgskw31uscm4, 1ukpmkewk4i92tjmhsvewi4y7, 5u2qgwuw3z1y0lrof60yf6264, c0qrxhefhmxkbq22tiejp3enn, d4s7ik8fgv8082v3x31hifzcc, ec1fxlvgtcxlf2nzremqcca7r, eg1hxxu2e77a7w2wv79quwaxl (Description, and Name for five of them). Samples:
  1ukpmkewk4i92tjmhsvewi4y7 expected 'Maintain an asset inventory of organization authorized and existing hardware, software, and media. Where possible, utilize automated tools to facilitate the discovery and ongoing tracking of such assets.'; got 'Keep a current record of the authorized resources an account holds, and discover the resources in use automatically.'
  c0qrxhefhmxkbq22tiejp3enn expected Name 'Unauthorized asset management' and Description about removing/quarantining/denying connectivity to unauthorized assets; got Name 'Asset ownership assignment' and 'Assign each inventoried asset an owner who is accountable for protecting it.'

ListDomains task#1 — divergence key keys=$.Domains[Arn=...] for 22 entries: six as .Description value_mismatch (d4msesd9vvmzmmuvlv06m92uq, 33mjpzadrlwo1by3c1012ai5i, 884eyhr622uy73h6i1mjb9lfb, avh1ncrx6oyndrbsv0rfywvat, eyrstdrz8kmo28x5j8vgal6au and one further domain) and sixteen as whole-entry differences. Samples:
  d4msesd9vvmzmmuvlv06m92uq expected 'This control domain focuses on asset management and the systematic tracking and maintenance of physical or digital assets throughout their lifecycle, including acquisition, utilization, and disposal. This reduces risks related to accidents, malfunctions, and other issues that may cause damage to property or harm to people.'; got 'Controls that inventory Amazon Web Services resources and keep that inventory current.'
  33mjpzadrlwo1by3c1012ai5i expected 'This control domain focuses on planning and preparation of procedures and resources to ensure the continued operation of critical business functions in the event of a disruption, and to facilitate the recovery of normal operations afterwards.'; got 'Controls that keep workloads available, and restore them after a disruption.'
  884eyhr622uy73h6i1mjb9lfb expected the data-protection paragraph; got 'Controls that protect the confidentiality and integrity of data at rest and in transit.'
  whole-entry differences at, among others, domain/01d780d3ff2c8fd25185836de, /262d5c3b7a044409c5bb65351, /42a34489dabde1c06b0d71d19, /fc76959dae9dbcca8c896fd77 (hex-shaped, emulator side) vs domain/1yz9ep0ymkt1vufn0y3kyyhzv, /22ctir35fm1yojdh1sr4iucq7, /70qzkmyi2s3oxheokg6jqqiy7, /bn9ck1b74fp4b4wdysq0fanpc (catalogued, recording side).

Model citation: Smithy controlcatalog — ListCommonControls POST /common-controls, 200, paginated (items CommonControls, required); ListDomains POST /domains, 200, paginated (items Domains, required); CommonControlSummary$Name/$Description and DomainSummary$Name/$Description are the records' published catalog text.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixtures controlcatalog-catalog-list-common-controls and controlcatalog-catalog-list-domains from trace set simparity-wire-aws-v1.
2. For causes 1-4: POST /common-controls with the fixture's CommonControlFilter for the step under test (task#1, then task#4, task#27, task#35) and, in each 200 response, compare the CommonControlSummary Name and Description at each Arn named above with the recorded response's values for the same Arn.
3. For cause 5: POST /domains (step task#1) and compare both the set of DomainSummary Arns and each entry's Description with the recording — six catalogued domains come back with re-written Descriptions, and sixteen ARNs are present on only one side.
4. Every comparison differs while status (200) and wire shape match. Re-replay all five steps (controlcatalog-catalog-list-common-controls#task#1, #task#4, #task#27, #task#35 and controlcatalog-catalog-list-domains#task#1) to confirm a fix.
