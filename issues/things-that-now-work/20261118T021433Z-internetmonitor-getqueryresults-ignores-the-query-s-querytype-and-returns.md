Issue-Source: trace-sweep-divergence
Issue-Trace: internetmonitor-query-workflow
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: internetmonitor-query-workflow
Issue-Shape-Id: com.amazonaws.internetmonitor#InternetMonitor20210603
Issue-Key: internetmonitor|GetQueryResults|2xx->2xx|$.Data
Issue-Key: internetmonitor|GetQueryResults|2xx->2xx|$.Fields[*]
Issue-Key: internetmonitor|GetQueryResults|2xx->2xx|$.Fields[*].Type
Issue-Commit: ff49e19c9aa8fb66fb59a980b572d4d41aa9e3fd

# GetQueryResults ignores the query's QueryType and returns a fixed 18-column field catalog with wrong column types

> _Note: this issue was identified against the code base at commit `ff49e19c9aa8fb66fb59a980b572d4d41aa9e3fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryResults

## Description
Cause 1 of 1: the GetQueryResults response body reports the wrong repository schema, because the handler emits one hard-coded column catalog for every QueryType instead of the field set the query's QueryType defines, and types several of those columns differently from AWS.

AWS: Smithy model services/service-com-amazonaws-internetmonitor-internetmonitor20210603 (internetmonitor, 2021-06-03), GetQueryResults GET /v20210603/Monitors/{MonitorName}/Queries/{QueryId}/Results, 200. Output members Fields (QueryFields, required) and Data (QueryData, required, list of list of String). QueryField is documented as a "name-data type pair, such as availability_score-float", and QueryField's own doc states "Each QueryType includes a specific set of fields and datatypes to retrieve data for" — i.e. the Fields list is a function of the QueryType the query was started with (StartQuery.QueryType, enum MEASUREMENTS | TOP_LOCATIONS | TOP_LOCATION_DETAILS | OVERALL_TRAFFIC_SUGGESTIONS | OVERALL_TRAFFIC_SUGGESTIONS_DETAILS | ROUTING_SUGGESTIONS). The model does not enumerate the per-type column lists, so for the exact list this is recorded behavior only: for the QueryType this fixture starts, the recorded AWS response reports exactly 8 fields, in order, with these name/type pairs — timestamp/integer, availability/float, performance/float, rtt_p50/bigint, rtt_p90/bigint, rtt_p95/bigint, bytes_in/bigint, bytes_out/bigint — and carries no Data member at all on this step (the field names and the type tokens 'integer', 'float', 'bigint' are the fixed part of the contract; no value in the recording is a requirement).

Emulator: status matches (200 vs 200) but the body does not. services/service-com-amazonaws-internetmonitor-internetmonitor20210603/src/ops/get_query_results.rs defines a single const COLUMNS catalog of 18 columns and serializes it unconditionally (`fields: COLUMNS.iter().map(...)`), consulting QueryType only to decide which ROWS are selected (selected_rows), never which COLUMNS are reported. Replay therefore shows: Fields[0] 'timestamp' typed 'timestamp' instead of 'integer'; 'availability' and 'performance' absent entirely (the catalog has 'availability_score'/'performance_score' instead); rtt_p50/rtt_p90/rtt_p95 typed 'float' instead of 'bigint'; bytes_in and bytes_out missing entirely; and 10+ extra columns reported that the recorded schema does not contain (as_name, as_number, country, subdivision, metro, city, latitude, longitude, status, service_location, availability_score, performance_score, percent_of_total_traffic_impacted, percent_of_client_location_impacted). The emulator also emits "Data": [] where the recorded response has no Data member on this step. Hypothesis: AWS omits Data (rather than sending an empty list) when the query's repository holds no rows yet; the required-member trait says only that AWS populates it when rows exist.

Why: Data rows are positional — "Data is returned in arrays, aligned with the Fields for the query" — so a client decodes column N of every row using Fields[N]. A client that starts a query, reads Fields and then indexes rows by that schema gets a different column count, a different column order, and different declared datatypes than on AWS: every value it reads is attributed to the wrong column, numeric columns declared 'float' where AWS declares 'bigint' are parsed with the wrong numeric type, and the fields it depends on (availability, performance, bytes_in, bytes_out) are simply not present, so dashboards and ETL built on the query interface either fail to find their columns or silently mis-map data. FilterParameters are validated against the same catalog, so a filter on a real AWS field name matches nothing while a filter on an emulator-only name is accepted.

Fix: the GetQueryResults handler's schema/serializer layer in services/service-com-amazonaws-internetmonitor-internetmonitor20210603/src/ops/get_query_results.rs — replace the single COLUMNS const with a per-QueryType column catalog selected from the stored QueryRecord's query_type, with each column's declared type matching AWS's token ('integer' for timestamp, 'float' for availability/performance, 'bigint' for the rtt_* and bytes_* columns), and have Data, the FilterParameters lookup and the page projection all read from that same per-type catalog so the three cannot disagree. Note the open issue "StartQuery succeeds on a monitor with no monitored resources instead of refusing it with BadRequestException" touches the same query workflow but concerns StartQuery admission, not the result schema, so it does not cover this.

Traces: internetmonitor-query-workflow#task#4 (GetQueryResults, 200 vs 200, body mismatch on $.Fields[*] and $.Data).

## Evidence
Fixture internetmonitor-query-workflow, step task#4, operation GetQueryResults, verdict mismatch, expected_status 200, actual_status 200.

Expected (recorded) Fields, in order: [timestamp/integer, availability/float, performance/float, rtt_p50/bigint, rtt_p90/bigint, rtt_p95/bigint, bytes_in/bigint, bytes_out/bigint]; no Data member present.

Actual (emulator) Fields, in order: [timestamp/timestamp, as_name/string, as_number/integer, country/string, subdivision/string, metro/string, city/string, latitude/float, longitude/float, status/string, service_location/string, availability_score/float, performance_score/float, ...] (18 columns from the const COLUMNS catalog), plus Data: [].

Replay detail excerpts: "$.Fields[0].Type: value_mismatch (expected 'integer', got 'timestamp')"; "$.Fields[1]: missing (expected {'Name': 'availability', 'Type': 'float'}, got {'Name': 'as_name', 'Type': 'string'})"; "$.Fields[3].Type: value_mismatch (expected 'bigint', got 'string')"; "$.Fields[6]: missing (expected {'Name': 'bytes_in', 'Type': 'bigint'}, ...)"; "$.Fields[8..14]: extra (expected None, got {...})"; "$.Data: type_mismatch (expected None, got [])".

Source: the response is built from a single unconditional catalog — get_query_results.rs lines 160-264 (const COLUMNS) and lines 562-569 (fields: COLUMNS.iter().map(...)), with QueryType consulted only in selected_rows (lines 378-428).

## Steps to Reproduce
1. Replay fixture internetmonitor-query-workflow against the emulator (replay_wire_corpus); step task#4 diverges.
2. Manually: CreateMonitor a monitor, then StartQuery on it (POST /v20210603/Monitors/{MonitorName}/Queries) with the fixture's QueryType and time window; note the returned QueryId.
3. GET /v20210603/Monitors/{MonitorName}/Queries/{QueryId}/Results.
4. Observe the response Fields list: 18 columns (timestamp/timestamp, as_name, as_number, country, subdivision, metro, city, latitude, longitude, status, service_location, availability_score, performance_score, percent_*, rtt_p50/float, ...) and "Data": [].
5. AWS returns the QueryType's own 8-column schema for this query — timestamp/integer, availability/float, performance/float, rtt_p50|rtt_p90|rtt_p95/bigint, bytes_in|bytes_out/bigint — and no Data member while the repository holds no rows. Repeat with a different QueryType: the emulator returns the identical Fields list, proving QueryType is not consulted for the schema.
