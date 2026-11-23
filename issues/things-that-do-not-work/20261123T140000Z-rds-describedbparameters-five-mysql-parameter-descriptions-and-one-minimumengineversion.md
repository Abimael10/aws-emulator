Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-parameters-pagination
Issue-Step: task#4
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-parameters-pagination
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].Description
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*].MinimumEngineVersion
Issue-Related: 20261119T201015Z-rds-describedbparameters-pages-serve-emulator-authored-parameter-metadata.md

# rds DescribeDBParameters: five MySQL parameter Descriptions and one MinimumEngineVersion on the first paged page still carry the emulator's own values, and the report that would settle them does not quote the catalog's

> _Split out of `issues/things-that-now-work/20261119T201015Z-rds-describedbparameters-pages-serve-emulator-authored-parameter-metadata.md`, whose causes 2 and 3 (the InnoDB compression page and the table-cache / adaptive-flushing / autoextend page) now replay clean, and whose cause 1 is closed for every row the parent report quotes AWS's text for. This report carries the rows of cause 1 that are NOT closed, re-filed so they are not recorded as working. Everything below is what the parent fix established about them; a later fixer starts from here rather than from scratch._

## Operation
DescribeDBParameters

## Description
The parent fix landed the rule and every row it had evidence for: `Parameter$Description` is the published parameter list's own text for the parameter NAME byte for byte, `Parameter$AllowedValues` carries a contiguous integer domain as the hyphenated range between its bounds (`0-2`) rather than as an enumeration of the values in it (`0,1,2`), and the user half of a report carries the same catalogue columns as the engine-default half. After it, the fixture's three paged steps diverge on ONE step only (task#4), the `AllowedValues` replay key is reported fixed, and task#5 and task#8 replay clean.

What remains is CONTENT this emulator has no source for: the published text of five MySQL parameters, and the `MinimumEngineVersion` the published list pins for one of them. The parent report lists the diverging paths of task#4 but quotes AWS's expected value only for the rows it describes in prose (indices 2, 3, 4, 5, 14, 15 and 16, all now served as recorded); for indices 6, 7, 10, 11 and 12 it says only that they "diverge in the same way", so the catalog text they must carry is not in the report and no rule over the model, the request or stored state yields it.

=== Cause 1 of 1: five Descriptions and one MinimumEngineVersion of the MySQL-family page are the emulator's own values ===
AWS: Smithy: DescribeDBParameters -> DBParameterGroupDetails{Parameters, Marker}; `com.amazonaws.rds#Parameter` members `Description` ('Provides a description of the parameter.') and `MinimumEngineVersion` ('The earliest engine version to which the parameter can apply.'). The model fixes the shape only: both strings are values of the engine's published parameter list, so they are fixed per-parameter content and not per-run values.
Emulator: on `rds-db-parameters-pagination#task#4` — the first page of the MySQL-family group's parameters, 20 rows from `auto_increment_increment` to `innodb_compression_failure_threshold_pct` — the rows at indices 6, 7, 10, 11 and 12 report the emulator's own summary of what the setting does instead of the published text, and the row at index 12 reports a `MinimumEngineVersion` the recording does not carry the same value for. Status is 200 -> 200, so nothing surfaces as an error.
Which rows they are: index 12 is the group-replication member expel timeout (`group_replication_member_expel_timeout`, the only entry of that span the catalogue pins a version for — the emulator reports `8.0.13`, the release the engine introduced the variable in, and that value is also what decides which FAMILIES carry the entry, so a fix must publish the reported version separately from the gating one, the way `CATALOG_PUBLISHED_FROM` already publishes a list-membership bound separately). Indices 6, 7, 10 and 11 are four of `connect_timeout`, `default_password_lifetime`, `default_week_format`, `delayed_insert_timeout`, `div_precision_increment`, `flush_time` and `general_log` — the span of the catalogue between `concurrent_insert` (index 5) and `host_cache_size` (index 13, which replays clean) — and the two of them at indices 8 and 9 already match the recording, so the emulator's text is right for two of the seven and wrong for five.
Why: a client that compares served parameter metadata against AWS's catalog (drift reporting, a console-style parameter browser, generated documentation, an upgrade-eligibility check reading `MinimumEngineVersion`) sees a difference on those rows of the first page it reads.
Fix: `MYSQL_PARAMETERS` in `services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_engine_default_parameters.rs` — the `description` column of those five entries, and a published column for the `MinimumEngineVersion` the list pins for the group-replication entry, separate from the introduction release the family gating reads (`crate::state::family_carries_parameter`). Nothing in the projection needs changing: it already reports the catalogue's columns on both halves of a report, which the parent fix pinned in `tests/all/db_parameter_catalog_text.rs`.
What a fixer needs: the recorded `Parameter[6]`, `[7]`, `[10]`, `[11]` and `[12]` of that step — each row's `ParameterName` with its `Description`, and `[12]`'s `MinimumEngineVersion` (or the absence of it). Writing a plausible sentence instead is what this report exists to prevent: the published text is a value of AWS's catalog, and the engine manual's own wording for the same variable is demonstrably NOT it for every entry (the list describes `innodb_compression_level` with the padding-threshold sentence, which the parent fix had to carry verbatim).
Traces: rds-db-parameters-pagination#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from the parent fix: verdict failed, 1 diverged step across 1 graded fixture (down from 3), `AllowedValues` key fixed, `Description` and `MinimumEngineVersion` keys still failing on `rds-db-parameters-pagination` alone. The reported divergence is

```
DescribeDBParameters (rds-db-parameters-pagination#task#4): HTTP 200 -> HTTP 200; differs at
  $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[10].Description,
  $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[11].Description,
  $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[12].Description, …
```

and the parent report's own path list for that step adds `Parameter[6].Description`, `Parameter[7].Description` and `Parameter[12].MinimumEngineVersion`. Every other path of that step, and every path of task#5 and task#8, now matches.

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1 against it (replay_wire_corpus).
3. Replay fixture rds-db-parameters-pagination step task#4 and read the recorded page: for each of `Parameter[6]`, `[7]`, `[10]`, `[11]` and `[12]`, take the `ParameterName` and the `Description` the recording carries, and `[12]`'s `MinimumEngineVersion`.
4. Write those values into the matching entries of `MYSQL_PARAMETERS`, with the reported version published separately from the introduction release the family gating compares against.
5. Re-replay task#4: it must carry no diverging path. task#5 and task#8 must stay clean.
