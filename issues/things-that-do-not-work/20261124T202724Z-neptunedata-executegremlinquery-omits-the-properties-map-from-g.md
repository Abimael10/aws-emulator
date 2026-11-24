Issue-Source: trace-sweep-divergence
Issue-Trace: neptunedata-query-languages-live-cluster-round-trip
Issue-Step: task#19
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptunedata-query-languages-live-cluster-round-trip
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata|ExecuteGremlinQuery|2xx->2xx|$.result.data.@value[*].@value.properties
Issue-Commit: b043e12d6ab74ba6f2a17a7b629ac061e3122435

# neptunedata ExecuteGremlinQuery omits the properties map from g:Vertex GraphSON results

> _Note: this issue was identified against the code base at commit `b043e12d6ab74ba6f2a17a7b629ac061e3122435`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteGremlinQuery

## Description
One cause.

Cause 1 of 1: a vertex returned by ExecuteGremlinQuery is serialized without its properties, so every property the store holds for that vertex is dropped from the answer. Fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#19`.

AWS: Smithy (com.amazonaws.neptunedata#ExecuteGremlinQuery) models `result` as a `Document` ("The Gremlin query output from the server") with HTTP POST /gremlin -> 200, and the input member `serializer` (httpHeader `accept`) selects the GraphSON form; the model does not spell out the GraphSON shape, so the vertex encoding itself is recorded behavior only. The recording shows Neptune answering 200 with the GraphSON 3.0 vertex encoding: `result.data` is a `g:List` whose members are `g:Vertex` objects whose `@value` carries `id`, `label` AND `properties`. `properties` is a map keyed by property name; each key's value is a list of `g:VertexProperty` objects whose `@value` carries `id` (a `g:Int32` — the per-property id, a generated value), `value` (the property's value) and `label` (the property name, equal to the map key). That is Apache TinkerPop GraphSON 3.0's vertex form, which Neptune's Gremlin endpoint answers in. Status 200 on both sides; no error code or message is involved in this divergence.

Emulator: answers 200 with a `g:Vertex` whose `@value` carries ONLY `id` and `label`. The `properties` key is absent entirely (replay detail: `$.result.data.@value[0].@value.properties: missing (expected a map of property name -> list of g:VertexProperty, got None)`). The properties ARE in the store — the same graph answers `values`/`valueMap` from `node.properties` — they are simply never written into the vertex's GraphSON.

Why: a client that reads a vertex's properties off the element returned by a traversal (`g.V()...`, the default GraphSON element form every TinkerPop/Gremlin client deserializes) sees a vertex with no properties at all. It cannot tell a vertex that genuinely has no properties from one whose properties were dropped, so round-trips that write with `addV().property(..)` and then read the element back appear to have lost the data, and any code keying off `vertex.properties` silently takes the empty path instead of failing loudly.

Fix: the Gremlin result serializer for this operation — the GraphSON writer in `services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/state/gremlin_eval.rs` (`Evaluation::graphson`, the `Item::Vertex` arm), which must emit the vertex's `properties` map built from the stored node's properties, each entry a list of `g:VertexProperty` values (`id`, `value`, `label`), with the existing typed-value writer (`graphson_value`) used for the property values. The handler itself (`src/ops/execute_gremlin_query.rs`) already hands the rendered stream to `state::result_document` unchanged, so nothing there needs to move. (The edge arm has the same gap in principle — `g:Edge` carries `properties` too — but only the vertex form is exercised by this fixture.)

Traces: fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#19` (ExecuteGremlinQuery, 200 -> 200, key `$.result.data.@value[0].@value.properties`).

Related but NOT covering this: open issue `20261104T225928Z-neptunedata-executegremlinquery-stub.md` asks for ExecuteGremlinQuery to be implemented at all and is stale with respect to this replay (the operation now answers 200, and the replay reports no stub for it); open issues `20261120T091858Z-neptunedata-getenginestatus-reports-labmode-tablespaceautoextendsizeinmb-as-a-switch.md` and `20261113T062625Z-neptunedata-executegremlinexplainquery-explain-report-diverges-from-neptune-s.md` own the other two divergences in this fixture and neither touches the query result encoding.

## Evidence
Replay of fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#19`, operation ExecuteGremlinQuery, verdict mismatch, expected_status 200, actual_status 200.

Divergence key: `service=neptunedata|ExecuteGremlinQuery|status=200->200|keys=$.result.data.@value[0].@value.properties`

Expected (recorded Neptune answer) at `$.result.data.@value[0].@value.properties`:
  {"<property name>": [{"@type": "g:VertexProperty", "@value": {"id": {"@type": "g:Int32", "@value": <generated per-property id>}, "value": "<the stored property value>", "label": "<property name>"}}]}
(the recording's instance used the probe's marker property; the vertex id, the property id and the property value are per-run values, the structure and the key names are the requirement)

Actual (emulator): the key is absent — `$.result.data.@value[0].@value` contains only `id` and `label`.

Code: `services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/state/gremlin_eval.rs` lines 1130-1138 build the `g:Vertex` `@value` from `id` and `self.label_of(item)` only; `properties` is never written, although `Evaluation::properties` (same file, lines 1081-1095) already reads the node's stored properties for `values`/`valueMap`.

## Steps to Reproduce
1. Boot the neptunedata emulator from this checkout.
2. POST /gremlin with a traversal that adds a vertex carrying a property, e.g. `g.addV('<label>').property('marker','<value>')`.
3. POST /gremlin with a traversal that returns that vertex as an element, e.g. `g.V().hasLabel('<label>')`.
4. Observe HTTP 200 whose `result.data.@value[0]` is a `g:Vertex` whose `@value` has only `id` and `label`; AWS returns the same vertex with a `properties` map holding a `g:VertexProperty` for `marker`.
5. Equivalently: replay fixture `neptunedata-query-languages-live-cluster-round-trip` and observe step `task#19` mismatch on `$.result.data.@value[0].@value.properties`.
