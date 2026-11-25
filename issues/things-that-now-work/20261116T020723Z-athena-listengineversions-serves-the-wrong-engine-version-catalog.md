Issue-Source: trace-sweep-divergence
Issue-Trace: athena-catalog-list-engine-versions
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-catalog-list-engine-versions athena-service-catalog-metadata-and-workgroup-conflicts
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|ListEngineVersions|2xx->2xx|$.EngineVersions[*]
Issue-Commit: f6f16de4d463b0ffce56b8f6eb39771090126fd8

# Athena ListEngineVersions serves the wrong engine-version catalog: no Category, an unexpected EffectiveEngineVersion, and a missing Spark entry

> _Note: this issue was identified against the code base at commit `f6f16de4d463b0ffce56b8f6eb39771090126fd8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListEngineVersions

## Description
ListEngineVersions is meant to enumerate the engine versions a caller may select for a workgroup or a session. The emulator serves a different catalog than Athena does, in both shape and content. 1 cause.

Cause 1 of 1: the EngineVersions list omits Category, carries EffectiveEngineVersion that Athena does not return on this listing, and is missing one of the selectable Spark versions.
AWS: Smithy: ListEngineVersions output EngineVersions (EngineVersionsList, @length 0..10) of EngineVersion; EngineVersion models SelectedEngineVersion and EffectiveEngineVersion (both NameString @length 1..128), paginated by NextToken/MaxResults (MaxEngineVersionsCount @range 1..10). The `Category` member is recorded behavior only — it is not in the on-disk model, but the recorded AWS response carries it on every entry. Recorded behavior: the response is a catalog of selectable versions, one entry per version, each entry carrying exactly a Category naming the engine family ('Presto' or 'Spark') and a SelectedEngineVersion naming the selectable value; no entry carries EffectiveEngineVersion, because nothing has been selected yet. The recorded catalog is the four entries Spark/'Apache Spark version 3.5', Presto/'AUTO', Presto/'Athena engine version 3' and Spark/'PySpark engine version 3' (status 200).
Emulator: status 200, but only three entries, and each is shaped as a resolved selection rather than a catalog row: {SelectedEngineVersion 'AUTO', EffectiveEngineVersion 'Athena engine version 3'}, {SelectedEngineVersion 'Athena engine version 3', EffectiveEngineVersion 'Athena engine version 3'}, {SelectedEngineVersion 'PySpark engine version 3', EffectiveEngineVersion 'PySpark engine version 3'}. Category is absent on every entry, EffectiveEngineVersion is present on every entry, and the Apache Spark version entry is absent from the catalog entirely.
Why: a client that calls ListEngineVersions to discover what it may put in CreateWorkGroup/UpdateWorkGroup EngineVersion.SelectedEngineVersion, or to pick a Spark version for a session, cannot separate the Presto family from the Spark family at all (no Category), and never sees the Apache Spark version as selectable, so a legitimate selection is rejected as unavailable by the client's own pre-validation. A client that reads EffectiveEngineVersion from this listing reads a value Athena does not supply here and treats an unselected catalog row as a resolved engine binding.
Fix: the athena crate's ListEngineVersions handler and the static engine-version catalog it serves — the catalog rows must carry Category plus SelectedEngineVersion and must not carry EffectiveEngineVersion, and must include the Apache Spark entry.
Traces: athena-catalog-list-engine-versions#task#1, athena-service-catalog-metadata-and-workgroup-conflicts#task#2.

Related but not covering this: open issues 20261111T064644Z-athena-listworkgroups-summaries-drop-description-and-engineversion-category.md and 20261114T204410Z-athena-workgroup-and-data-catalog-reads-still-drop.md concern EngineVersion.Category on ListWorkGroups/GetWorkGroup responses, not the ListEngineVersions catalog; 20261104T163600Z-athena-result-and-statistics-representation-is-wrong-boolean.md concerns ListQueryExecutions. Neither owns this replay key.

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the emulator built from this checkout.

Fixture athena-catalog-list-engine-versions, step task#1 (ListEngineVersions), expected status 200, actual status 200:
  $.EngineVersions[0]: expected {'Category': 'Spark', 'SelectedEngineVersion': 'Apache Spark version 3.5'}, got {'SelectedEngineVersion': 'AUTO', 'EffectiveEngineVersion': 'Athena engine version 3'}
  $.EngineVersions[1]: expected {'Category': 'Presto', 'SelectedEngineVersion': 'AUTO'}, got {'SelectedEngineVersion': 'Athena engine version 3', 'EffectiveEngineVersion': 'Athena engine version 3'}
  $.EngineVersions[2]: expected {'Category': 'Presto', 'SelectedEngineVersion': 'Athena engine version 3'}, got {'SelectedEngineVersion': 'PySpark engine version 3', 'EffectiveEngineVersion': 'PySpark engine version 3'}
  $.EngineVersions[3]: expected {'Category': 'Spark', 'SelectedEngineVersion': 'PySpark engine version 3'}, got None (the emulator returns 3 entries, AWS 4)

The same divergence reproduces at athena-service-catalog-metadata-and-workgroup-conflicts#task#2.

No status or error-code divergence is involved: both sides answer 200. The divergence is purely the body's member set (Category missing, EffectiveEngineVersion extra) and the catalog's contents (Apache Spark version 3.5 absent).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from this checkout.
2. Replay wire fixture athena-catalog-list-engine-versions and inspect step task#1 (also athena-service-catalog-metadata-and-workgroup-conflicts step task#2).
3. Minimal manual repro: call athena ListEngineVersions with no parameters. Every entry of EngineVersions must carry Category ('Presto' or 'Spark') together with SelectedEngineVersion and must NOT carry EffectiveEngineVersion, and the list must include a Spark entry whose SelectedEngineVersion is the Apache Spark version alongside the Presto 'AUTO', Presto engine-version and Spark PySpark entries.
