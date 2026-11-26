// SPDX-License-Identifier: CC0-1.0

//! `GetDataflowGraph`: the directed acyclic graph a Python script describes.
//!
//! RULE: a Glue ETL script carries its own record of its graph in the annotation comments
//! codegen writes above each node — `@type` (what the node does), `@args` (its properties,
//! as the script spells them), `@return` (the name the node binds, which is its identifier)
//! and `@inputs` (the frames it reads, each under the parameter it is passed as) — and this
//! operation reads those annotations back: one `CodeGenNode` per annotated node, carrying
//! its properties as name-value pairs and the line its own annotation block opens on, and one
//! `CodeGenEdge` per input, from the node it names to the node reading it under that
//! parameter. An argument's `Value` is the text the script spells it with, quotes and
//! brackets included, and `Param` is set where that text is a reference the script resolves
//! rather than a literal. A script annotating no node describes the empty graph. A script
//! whose annotations cannot describe a graph the model admits — a node with no `@return`, a
//! name `CodeGenIdentifier` does not admit, a name bound twice, more properties than one
//! node may carry — is refused with `InvalidInputException`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: how many nodes a script annotates
//! and how they are chained, how the comment carrying an annotation is spelled and
//! indented, which annotations a node carries at all, how an argument's value is spelled
//! (quoted either way, a list of tuples, a dict, a number, a keyword, a job argument, a
//! variable), how many frames a node reads and whether each is named by parameter or
//! positionally, whether the annotations have a statement of their own, and whether the
//! script annotates anything at all. Two further tables walk each bound the answer's own
//! shapes publish, at the bound and one past it.

use serde_json::{json, Value};

use crate::error_messages::assert_refusal;
use crate::wire::{emulator, ok};

/// One expected argument of a node: its `Name`, the `Value` text and whether `Param` is set.
type Arg = (&'static str, &'static str, bool);

/// One expected node of the graph.
struct Node {
    id: &'static str,
    node_type: &'static str,
    line: i64,
    args: Vec<Arg>,
}

/// One expected edge of the graph.
struct Edge {
    source: &'static str,
    target: &'static str,
    parameter: Option<&'static str>,
}

/// One row: a script and the graph it describes.
struct Row {
    case: &'static str,
    script: &'static str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

/// The node of a graph as a comparable row: `Id`, `NodeType`, `LineNumber` and each
/// argument's `Name`, `Value` and `Param`. The two members carrying a modelled default
/// (`LineNumber`, `Param`) are read as that default when the answer omits them.
type NodeRow = (String, String, i64, Vec<(String, String, bool)>);

/// The nodes an answer reports, as comparable rows in the order it reports them.
fn node_rows(case: &str, answered: &Value) -> Vec<NodeRow> {
    answered
        .get("DagNodes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must report DagNodes, got {answered}"))
        .iter()
        .map(|node| {
            let args = node
                .get("Args")
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every node must report its Args, got {node}")
                })
                .iter()
                .map(|arg| {
                    (
                        arg["Name"].as_str().unwrap_or_default().to_string(),
                        arg["Value"].as_str().unwrap_or_default().to_string(),
                        arg.get("Param").and_then(Value::as_bool).unwrap_or(false),
                    )
                })
                .collect();
            (
                node["Id"].as_str().unwrap_or_default().to_string(),
                node["NodeType"].as_str().unwrap_or_default().to_string(),
                node.get("LineNumber").and_then(Value::as_i64).unwrap_or(0),
                args,
            )
        })
        .collect()
}

/// The edges an answer reports, as `Source`, `Target` and `TargetParameter`.
fn edge_rows(case: &str, answered: &Value) -> Vec<(String, String, Option<String>)> {
    answered
        .get("DagEdges")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must report DagEdges, got {answered}"))
        .iter()
        .map(|edge| {
            (
                edge["Source"].as_str().unwrap_or_default().to_string(),
                edge["Target"].as_str().unwrap_or_default().to_string(),
                edge.get("TargetParameter")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect()
}

/// The same rows, from a row's expectation.
fn expected_nodes(row: &Row) -> Vec<NodeRow> {
    row.nodes
        .iter()
        .map(|node| {
            (
                node.id.to_string(),
                node.node_type.to_string(),
                node.line,
                node.args
                    .iter()
                    .map(|(name, value, param)| (name.to_string(), value.to_string(), *param))
                    .collect(),
            )
        })
        .collect()
}

/// The same for a row's expected edges.
fn expected_edges(row: &Row) -> Vec<(String, String, Option<String>)> {
    row.edges
        .iter()
        .map(|edge| {
            (
                edge.source.to_string(),
                edge.target.to_string(),
                edge.parameter.map(str::to_string),
            )
        })
        .collect()
}

/// The rows: one per shape of annotated script the rule covers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the canonical source node Glue codegen writes",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [database = \"legislators\", table_name = \"persons_json\", \
                 transformation_ctx = \"datasource0\"]\n",
                "## @return: datasource0\n",
                "## @inputs: []\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog(database = \
                 \"legislators\", table_name = \"persons_json\")\n",
            ),
            nodes: vec![Node {
                id: "datasource0",
                node_type: "DataSource",
                line: 1,
                args: vec![
                    ("database", "\"legislators\"", false),
                    ("table_name", "\"persons_json\"", false),
                    ("transformation_ctx", "\"datasource0\"", false),
                ],
            }],
            edges: vec![],
        },
        Row {
            case: "a chain of two nodes, annotated in single-hash comments and indented",
            script: concat!(
                "# @type: DataSource\n",
                "# @return: datasource0\n",
                "# @inputs: []\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog(database = \"db\")\n",
                "\n",
                "  # @type: ApplyMapping\n",
                "  # @args: [mapping = [(\"id\", \"int\", \"id\", \"long\"), (\"name\", \
                 \"string\", \"name\", \"string\")], transformation_ctx = \"applymapping1\"]\n",
                "  # @return: applymapping1\n",
                "  # @inputs: [frame = datasource0]\n",
                "  applymapping1 = ApplyMapping.apply(frame = datasource0)\n",
            ),
            nodes: vec![
                Node {
                    id: "datasource0",
                    node_type: "DataSource",
                    line: 1,
                    args: vec![],
                },
                Node {
                    id: "applymapping1",
                    node_type: "ApplyMapping",
                    line: 6,
                    args: vec![
                        (
                            "mapping",
                            "[(\"id\", \"int\", \"id\", \"long\"), (\"name\", \"string\", \
                             \"name\", \"string\")]",
                            false,
                        ),
                        ("transformation_ctx", "\"applymapping1\"", false),
                    ],
                },
            ],
            edges: vec![Edge {
                source: "datasource0",
                target: "applymapping1",
                parameter: Some("frame"),
            }],
        },
        Row {
            case: "a value the script resolves is a parameter, a value it states is not",
            script: concat!(
                "## @type: DataSink\n",
                "## @args: [table_name = args[\"TARGET_TABLE\"], path = target_path, \
                 partitions = 4, overwrite = True, format = \"json\", absent = None]\n",
                "## @return: datasink2\n",
                "## @inputs: [frame = applymapping1]\n",
                "datasink2 = glueContext.write_dynamic_frame.from_options(frame = \
                 applymapping1)\n",
            ),
            nodes: vec![Node {
                id: "datasink2",
                node_type: "DataSink",
                line: 1,
                args: vec![
                    ("table_name", "args[\"TARGET_TABLE\"]", true),
                    ("path", "target_path", true),
                    ("partitions", "4", false),
                    ("overwrite", "True", false),
                    ("format", "\"json\"", false),
                    ("absent", "None", false),
                ],
            }],
            edges: vec![Edge {
                source: "applymapping1",
                target: "datasink2",
                parameter: Some("frame"),
            }],
        },
        Row {
            case: "a dict value carrying commas, and a value quoted the other way",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [connection_options = {\"paths\": [\"s3://b/a\", \"s3://b/c\"], \
                 \"recurse\": True}, format = 'csv']\n",
                "## @return: datasource0\n",
                "## @inputs: []\n",
                "datasource0 = glueContext.create_dynamic_frame.from_options()\n",
            ),
            nodes: vec![Node {
                id: "datasource0",
                node_type: "DataSource",
                line: 1,
                args: vec![
                    (
                        "connection_options",
                        "{\"paths\": [\"s3://b/a\", \"s3://b/c\"], \"recurse\": True}",
                        false,
                    ),
                    ("format", "'csv'", false),
                ],
            }],
            edges: vec![],
        },
        Row {
            case: "a node reading two frames, each under the parameter it is read as",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: datasource0\n",
                "## @inputs: []\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog()\n",
                "## @type: DataSource\n",
                "## @return: datasource1\n",
                "## @inputs: []\n",
                "datasource1 = glueContext.create_dynamic_frame.from_catalog()\n",
                "## @type: Join\n",
                "## @args: [keys1 = [\"id\"], keys2 = [\"id\"]]\n",
                "## @return: join2\n",
                "## @inputs: [frame1 = datasource0, frame2 = datasource1]\n",
                "join2 = Join.apply(frame1 = datasource0, frame2 = datasource1)\n",
            ),
            nodes: vec![
                Node {
                    id: "datasource0",
                    node_type: "DataSource",
                    line: 1,
                    args: vec![],
                },
                Node {
                    id: "datasource1",
                    node_type: "DataSource",
                    line: 5,
                    args: vec![],
                },
                Node {
                    id: "join2",
                    node_type: "Join",
                    line: 9,
                    args: vec![("keys1", "[\"id\"]", false), ("keys2", "[\"id\"]", false)],
                },
            ],
            edges: vec![
                Edge {
                    source: "datasource0",
                    target: "join2",
                    parameter: Some("frame1"),
                },
                Edge {
                    source: "datasource1",
                    target: "join2",
                    parameter: Some("frame2"),
                },
            ],
        },
        Row {
            case: "a frame named positionally is an edge under no parameter",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: datasource0\n",
                "## @inputs: []\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog()\n",
                "## @type: SelectFields\n",
                "## @args: [paths = [\"id\"]]\n",
                "## @return: selectfields1\n",
                "## @inputs: [datasource0]\n",
                "selectfields1 = SelectFields.apply(frame = datasource0, paths = [\"id\"])\n",
            ),
            nodes: vec![
                Node {
                    id: "datasource0",
                    node_type: "DataSource",
                    line: 1,
                    args: vec![],
                },
                Node {
                    id: "selectfields1",
                    node_type: "SelectFields",
                    line: 5,
                    args: vec![("paths", "[\"id\"]", false)],
                },
            ],
            edges: vec![Edge {
                source: "datasource0",
                target: "selectfields1",
                parameter: None,
            }],
        },
        Row {
            case: "annotations with no statement of their own stand on the line they open",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: datasource0\n",
                "## @inputs: []\n",
                "## @type: DataSink\n",
                "## @return: datasink1\n",
                "## @inputs: [frame = datasource0]\n",
            ),
            nodes: vec![
                Node {
                    id: "datasource0",
                    node_type: "DataSource",
                    line: 1,
                    args: vec![],
                },
                Node {
                    id: "datasink1",
                    node_type: "DataSink",
                    line: 4,
                    args: vec![],
                },
            ],
            edges: vec![Edge {
                source: "datasource0",
                target: "datasink1",
                parameter: Some("frame"),
            }],
        },
        Row {
            case: "blank lines and the statement under the block leave the node's line alone",
            script: concat!(
                "## @type: DataSource\n",
                "\n",
                "## @return: datasource0\n",
                "## @args: []\n",
                "\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog()\n",
                "job.commit()\n",
            ),
            nodes: vec![Node {
                id: "datasource0",
                node_type: "DataSource",
                line: 1,
                args: vec![],
            }],
            edges: vec![],
        },
        Row {
            case: "a script that annotates no node describes the empty graph",
            script: concat!(
                "import sys\n",
                "from awsglue.context import GlueContext\n",
                "glueContext = GlueContext(SparkContext())\n",
                "job.commit()\n",
            ),
            nodes: vec![],
            edges: vec![],
        },
        Row {
            case: "an empty script describes the empty graph",
            script: "",
            nodes: vec![],
            edges: vec![],
        },
    ]
}

/// Every row's graph is the one its script's annotations record.
#[tokio::test]
async fn the_graph_is_the_one_the_scripts_annotations_record() {
    let router = emulator();
    for row in &rows() {
        let answered = ok(
            &router,
            "GetDataflowGraph",
            &json!({ "PythonScript": row.script }),
        )
        .await;
        let nodes = node_rows(row.case, &answered);
        let expected = expected_nodes(row);
        assert_eq!(
            nodes, expected,
            "case {case:?}: expected the nodes {expected:?}, got {nodes:?}",
            case = row.case,
        );
        let edges = edge_rows(row.case, &answered);
        let expected = expected_edges(row);
        assert_eq!(
            edges, expected,
            "case {case:?}: expected the edges {expected:?}, got {edges:?}",
            case = row.case,
        );
    }
}

/// A request stating no script at all states no graph: both lists are reported, and empty.
#[tokio::test]
async fn a_request_with_no_script_is_served_the_empty_graph() {
    let router = emulator();
    let answered = ok(&router, "GetDataflowGraph", &json!({})).await;
    assert_eq!(
        node_rows("no script", &answered),
        Vec::<NodeRow>::new(),
        "a request stating no script must report no node, got {answered}"
    );
    assert_eq!(
        edge_rows("no script", &answered),
        Vec::new(),
        "a request stating no script must report no edge, got {answered}"
    );
}

/// The database the generated-plan round trip stores its tables in.
const DATABASE: &str = "graph_db";

/// The graph of a script this emulator GENERATED is that script's own pipeline: the plan's
/// nodes in order, chained by the frame each reads, with every node's `LineNumber` naming
/// the line its annotation block opens on — which is what makes the graph a reading of
/// the script rather than a restatement of the request.
#[tokio::test]
async fn the_graph_of_a_generated_plan_is_that_plans_pipeline() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    for table in ["orders", "orders_copy"] {
        ok(
            &router,
            "CreateTable",
            &json!({
                "DatabaseName": DATABASE,
                "TableInput": {
                    "Name": table,
                    "StorageDescriptor": { "Columns": [{ "Name": "id", "Type": "int" }] },
                },
            }),
        )
        .await;
    }
    let planned = ok(
        &router,
        "GetPlan",
        &json!({
            "Source": { "DatabaseName": DATABASE, "TableName": "orders" },
            "Sinks": [{ "DatabaseName": DATABASE, "TableName": "orders_copy" }],
            "Mapping": [{ "SourcePath": "id" }],
        }),
    )
    .await;
    let script = planned["PythonScript"]
        .as_str()
        .unwrap_or_else(|| panic!("GetPlan must answer a PythonScript, got {planned}"))
        .to_string();

    let answered = ok(
        &router,
        "GetDataflowGraph",
        &json!({ "PythonScript": script }),
    )
    .await;
    let nodes = node_rows("a generated plan", &answered);

    let expected_ids = [
        ("datasource0", "DataSource"),
        ("applymapping1", "ApplyMapping"),
        ("selectfields2", "SelectFields"),
        ("resolvechoice3", "ResolveChoice"),
        ("datasink4", "DataSink"),
    ];
    let identified: Vec<(String, String)> = nodes
        .iter()
        .map(|(id, node_type, _, _)| (id.clone(), node_type.clone()))
        .collect();
    let expected: Vec<(String, String)> = expected_ids
        .iter()
        .map(|(id, node_type)| (id.to_string(), node_type.to_string()))
        .collect();
    assert_eq!(
        identified, expected,
        "the graph of the generated plan must be its pipeline; expected {expected:?}, got \
         {identified:?}"
    );

    // Every node stands on the line its own annotation block opens on.
    let lines: Vec<&str> = script.lines().collect();
    for (id, node_type, line, args) in &nodes {
        let at = usize::try_from(*line).expect("a line number is positive");
        let spelled = lines
            .get(at.saturating_sub(1))
            .unwrap_or_else(|| panic!("node {id} names line {line}, past the script's end"));
        assert_eq!(
            spelled.trim(),
            format!("## @type: {node_type}"),
            "node {id} must stand on the line its annotation block opens on; line {line} is \
             {spelled:?}"
        );
        // Every node of a generated plan carries the transformation context naming it.
        assert!(
            args.contains(&(
                "transformation_ctx".to_string(),
                format!("\"{id}\""),
                false
            )),
            "node {id} must carry its transformation context, got the arguments {args:?}"
        );
    }

    // The source node reads the stored table out of the catalog, as the script spells it.
    let (_, _, _, source_args) = &nodes[0];
    for expected in [
        ("database".to_string(), format!("\"{DATABASE}\""), false),
        ("table_name".to_string(), "\"orders\"".to_string(), false),
    ] {
        assert!(
            source_args.contains(&expected),
            "the source node must carry {expected:?}, got {source_args:?}"
        );
    }

    // The pipeline is chained: each node reads the frame the node before it bound.
    let edges = edge_rows("a generated plan", &answered);
    let expected: Vec<(String, String, Option<String>)> = expected_ids
        .windows(2)
        .map(|pair| {
            (
                pair[0].0.to_string(),
                pair[1].0.to_string(),
                Some("frame".to_string()),
            )
        })
        .collect();
    assert_eq!(
        edges, expected,
        "the plan's nodes must be chained by the frame each reads; expected {expected:?}, got \
         {edges:?}"
    );
}

/// Transforming a script is a REPORT: two identical calls answer the same graph, and a
/// transform never touches what the catalog holds.
#[tokio::test]
async fn transforming_a_script_changes_nothing() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    ok(
        &router,
        "CreateTable",
        &json!({ "DatabaseName": DATABASE, "TableInput": { "Name": "orders" } }),
    )
    .await;
    let read = json!({ "DatabaseName": DATABASE, "Name": "orders" });
    let before = ok(&router, "GetTable", &read).await;

    for row in &rows() {
        let request = json!({ "PythonScript": row.script });
        let first = ok(&router, "GetDataflowGraph", &request).await;
        let again = ok(&router, "GetDataflowGraph", &request).await;
        assert_eq!(
            first, again,
            "case {case:?}: two identical calls must answer the same graph; got {first} then \
             {again}",
            case = row.case,
        );
    }

    let after = ok(&router, "GetTable", &read).await;
    assert_eq!(
        before, after,
        "transforming a script must leave the catalog alone; expected {before}, got {after}"
    );
}

/// A script whose annotations cannot describe a graph the answer's own shapes admit is
/// refused with HTTP 400 and this operation's modelled `InvalidInputException`, naming what
/// in the script cannot be read as part of a graph.
#[tokio::test]
async fn a_script_that_describes_no_graph_is_refused() {
    struct Miss {
        case: &'static str,
        script: String,
        message: String,
    }

    let misses = vec![
        Miss {
            case: "a node the script annotates without naming",
            script: concat!(
                "## @type: DataSource\n",
                "## @inputs: []\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog()\n",
            )
            .to_string(),
            message: "the node annotated @type: DataSource at line 1 carries no @return naming it"
                .to_string(),
        },
        Miss {
            case: "a node name beginning with a digit",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: 0datasource\n",
                "datasource = glueContext.create_dynamic_frame.from_catalog()\n",
            )
            .to_string(),
            message: "0datasource at line 1 is not a node identifier a dataflow graph may carry"
                .to_string(),
        },
        Miss {
            case: "a node name carrying punctuation the identifier admits none of",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: data-source\n",
                "data_source = glueContext.create_dynamic_frame.from_catalog()\n",
            )
            .to_string(),
            message: "data-source at line 1 is not a node identifier a dataflow graph may carry"
                .to_string(),
        },
        Miss {
            case: "two nodes the script names the same",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: datasource0\n",
                "datasource0 = glueContext.create_dynamic_frame.from_catalog()\n",
                "## @type: DataSink\n",
                "## @return: datasource0\n",
                "datasource0 = glueContext.write_dynamic_frame.from_catalog()\n",
            )
            .to_string(),
            message: "datasource0 at line 4 names a node the script already names".to_string(),
        },
        Miss {
            case: "a frame the script reads under a name no node may carry",
            script: concat!(
                "## @type: ApplyMapping\n",
                "## @return: applymapping1\n",
                "## @inputs: [frame = 9datasource]\n",
                "applymapping1 = ApplyMapping.apply(frame = datasource0)\n",
            )
            .to_string(),
            message: "9datasource at line 1 is not a node identifier a dataflow graph may carry"
                .to_string(),
        },
    ];

    let router = emulator();
    for miss in &misses {
        assert_refusal(
            &router,
            miss.case,
            "GetDataflowGraph",
            &json!({ "PythonScript": miss.script }),
            "InvalidInputException",
            &miss.message,
        )
        .await;
    }

    // A refused script refuses nothing else: a script that does describe a graph is still
    // transformed afterwards, so the refusals above cannot be a handler that refuses
    // everything.
    let served = ok(
        &router,
        "GetDataflowGraph",
        &json!({
            "PythonScript": "## @type: DataSource\n## @return: datasource0\ndatasource0 = x()\n",
        }),
    )
    .await;
    assert_eq!(
        node_rows("after the refusals", &served).len(),
        1,
        "a script describing one node must still be transformed, got {served}"
    );
}

/// A script whose annotated TEXT stops reading as a list of entries is refused where it
/// stopped: the entries of an annotated list are separated by commas, so a second binding
/// opening inside one entry is a list that does not parse, and the refusal names the
/// position of the token it stopped at and quotes the text from the start of that node's
/// annotation block up to and including it.
///
/// The rows differ along what the rule generalises over: which annotated list carries the
/// missing separator (`@args`, `@inputs`), where the node's block stands in the script (at
/// the first line, after a preamble, as the second of two blocks), and how many entries
/// stand before the malformed one.
#[tokio::test]
async fn a_list_whose_entries_lose_their_separator_is_refused_where_it_stopped() {
    /// One row: the script, and the position and quoted span the refusal must carry.
    struct Stopped {
        case: &'static str,
        script: String,
        line: usize,
        column: usize,
        /// The text the refusal quotes, with the newlines escaped as the refusal escapes
        /// them.
        quoted: &'static str,
    }

    let rows = vec![
        Stopped {
            case: "a missing comma between two @args entries",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [database = \"example_db\" table_name = \"persons\"]\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            line: 2,
            // `## @args: [` is eleven characters, then `database = "example_db" ` is
            // twenty-four more, so the token that opens the second entry starts at 35.
            column: 35,
            quoted: "## @type: DataSource\\n## @args: [database = \"example_db\" table_name",
        },
        Stopped {
            case: "a missing comma between two @inputs entries",
            script: concat!(
                "## @type: Join\n",
                "## @return: join2\n",
                "## @inputs: [frame1 = datasource0 frame2 = datasource1]\n",
                "join2 = Join.apply()\n",
            )
            .to_string(),
            line: 3,
            column: 34,
            quoted: "## @type: Join\\n## @return: join2\\n## @inputs: [frame1 = datasource0 \
                     frame2",
        },
        Stopped {
            case: "a block that stands after a job skeleton, not at the first line",
            script: concat!(
                "import sys\n",
                "from awsglue.job import Job\n",
                "\n",
                "## @type: DataSource\n",
                "## @args: [database = \"example_db\" table_name = \"persons\"]\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            line: 5,
            column: 35,
            quoted: "## @type: DataSource\\n## @args: [database = \"example_db\" table_name",
        },
        Stopped {
            case: "the SECOND block of the script is the one that stops parsing",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [database = \"example_db\"]\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
                "## @type: DataSink\n",
                "## @args: [path = \"s3://b/out/\" format = \"json\"]\n",
                "## @return: datasink1\n",
                "datasink1 = y()\n",
            )
            .to_string(),
            line: 6,
            // `## @args: [` is eleven characters, then `path = "s3://b/out/" ` is
            // twenty-one more, so the token that opens the second entry starts at 32.
            column: 32,
            quoted: "## @type: DataSink\\n## @args: [path = \"s3://b/out/\" format",
        },
        Stopped {
            case: "two entries stand before the malformed one",
            script: concat!(
                "## @type: DataSink\n",
                "## @args: [a = 1, b = 2, c = 3 d = 4]\n",
                "## @return: datasink0\n",
                "datasink0 = y()\n",
            )
            .to_string(),
            line: 2,
            column: 31,
            quoted: "## @type: DataSink\\n## @args: [a = 1, b = 2, c = 3 d",
        },
    ];

    let router = emulator();
    for row in &rows {
        assert_refusal(
            &router,
            row.case,
            "GetDataflowGraph",
            &json!({ "PythonScript": row.script }),
            "InvalidInputException",
            &format!(
                "line {}:{} no viable alternative at input '{}'",
                row.line, row.column, row.quoted
            ),
        )
        .await;
    }
}

/// What a comma-less juxtaposition is NOT: an annotated list whose entry merely CARRIES an
/// `=` inside a quoted string, a bracket or a comparison is one entry and is SERVED, and so
/// is a list whose entries are named positionally. The separator rule is about a second
/// BINDING opening inside an entry, and nothing else.
///
/// The rows differ along where the `=` stands: inside a quoted value, inside a bracketed
/// one, as a comparison operator, as part of a job-argument subscript, and in an entry that
/// binds no name at all.
#[tokio::test]
async fn an_entry_that_merely_carries_an_equals_sign_is_still_served() {
    let router = emulator();

    // (case, the `@args` text, the names the served node must carry)
    let rows: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "an `=` inside a quoted value",
            "[predicate = \"year = 2026\", path = \"s3://b/a=b/\"]".to_string(),
            vec!["predicate", "path"],
        ),
        (
            "an `=` inside a bracketed value",
            "[options = {\"filter\": \"dt = '2024-01-01'\"}]".to_string(),
            vec!["options"],
        ),
        (
            "a comparison, which binds nothing",
            "[predicate = year == 2026]".to_string(),
            vec!["predicate"],
        ),
        (
            "a job argument the script resolves",
            "[table_name = args[\"TARGET_TABLE\"]]".to_string(),
            vec!["table_name"],
        ),
        (
            "an entry that binds no name at all",
            "[database, table_name = \"persons\"]".to_string(),
            vec!["table_name"],
        ),
    ];

    for (case, args, expected) in &rows {
        let script = format!(
            "## @type: DataSource\n## @args: {args}\n## @return: datasource0\n\
             datasource0 = x()\n"
        );
        let answered = ok(
            &router,
            "GetDataflowGraph",
            &json!({ "PythonScript": script }),
        )
        .await;
        let nodes = node_rows(case, &answered);
        let names: Vec<String> = nodes
            .first()
            .map(|(_, _, _, args)| args.iter().map(|(name, _, _)| name.clone()).collect())
            .unwrap_or_default();
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected the served node to carry the arguments {expected:?}, got \
             {answered}"
        );
    }

    // And a positional `@inputs` entry beside a named one: an edge under no parameter is
    // not a list that lost a separator.
    let answered = ok(
        &router,
        "GetDataflowGraph",
        &json!({
            "PythonScript": concat!(
                "## @type: Join\n",
                "## @return: join2\n",
                "## @inputs: [datasource0, frame2 = datasource1]\n",
                "join2 = Join.apply()\n",
            ),
        }),
    )
    .await;
    assert_eq!(
        edge_rows("a positional input beside a named one", &answered),
        vec![
            ("datasource0".to_string(), "join2".to_string(), None),
            (
                "datasource1".to_string(),
                "join2".to_string(),
                Some("frame2".to_string())
            ),
        ],
        "a positional input must still be read as an edge, got {answered}"
    );
}

/// A script annotating a node with a name AT the length `CodeGenIdentifier` admits is served,
/// and one character past it is refused: the bound is the member's own, from
/// `smithy_query shape=CodeGenIdentifier` (length 1..255).
#[tokio::test]
async fn the_identifier_length_bound_admits_the_ceiling_and_refuses_past_it() {
    let router = emulator();
    for (case, length, admitted) in [
        ("one character", 1usize, true),
        ("at the 255 ceiling", 255, true),
        ("one past the ceiling", 256, false),
    ] {
        let name = "n".repeat(length);
        let script = format!("## @type: DataSource\n## @return: {name}\n{name} = x()\n");
        let request = json!({ "PythonScript": script });
        if admitted {
            let answered = ok(&router, "GetDataflowGraph", &request).await;
            let nodes = node_rows(case, &answered);
            assert_eq!(
                nodes.len(),
                1,
                "case {case:?}: a name of {length} characters must name a node, got {answered}"
            );
            assert_eq!(
                nodes[0].0, name,
                "case {case:?}: the node must be named as the script names it, got {:?}",
                nodes[0].0
            );
        } else {
            assert_refusal(
                &router,
                case,
                "GetDataflowGraph",
                &request,
                "InvalidInputException",
                &format!("{name} at line 1 is not a node identifier a dataflow graph may carry"),
            )
            .await;
        }
    }
}

/// A malformed, truncated or oversized script is ANSWERED — served as the graph its
/// readable annotations describe, or refused with the modelled `InvalidInputException` where
/// it is past a bound this operation enforces — and never read without end: the annotations
/// are read, never evaluated, so an unterminated quote, an unclosed bracket or an annotation
/// cut off mid-word is text that describes no further part of the graph.
#[tokio::test]
async fn a_malformed_truncated_or_oversized_script_is_answered() {
    use service_com_amazonaws_glue_awsglue::ops::get_dataflow_graph::{
        MAX_ANNOTATION_NESTING, MAX_SCRIPT_BYTES,
    };

    /// What one malformed script is answered with: the nodes it still describes, each as its
    /// `Id` and the names of the arguments it carries.
    struct Served {
        case: &'static str,
        script: String,
        nodes: Vec<(&'static str, Vec<&'static str>)>,
    }

    let served = vec![
        Served {
            case: "a value whose quote is never closed",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [database = \"unterminated, table_name = \"t\"]\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            nodes: vec![("datasource0", vec!["database"])],
        },
        Served {
            case: "a value whose bracket is never closed",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [paths = [\"a\", \"b\"\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            nodes: vec![("datasource0", vec!["paths"])],
        },
        Served {
            case: "an args annotation truncated to its opening bracket",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            nodes: vec![("datasource0", vec![])],
        },
        Served {
            case: "a script truncated inside an annotation's name",
            script: concat!(
                "## @type: DataSource\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
                "## @ret",
            )
            .to_string(),
            nodes: vec![("datasource0", vec![])],
        },
        Served {
            case: "an entry that spells no name binds no property",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [database]\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            nodes: vec![("datasource0", vec![])],
        },
        Served {
            case: "a comparison inside a value is not the binding",
            script: concat!(
                "## @type: DataSource\n",
                "## @args: [predicate = year == 2026]\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            nodes: vec![("datasource0", vec!["predicate"])],
        },
        Served {
            case: "a type annotation carrying no type",
            script: concat!(
                "## @type:\n",
                "## @return: datasource0\n",
                "datasource0 = x()\n",
            )
            .to_string(),
            nodes: vec![("datasource0", vec![])],
        },
        Served {
            case: "an annotation nesting brackets up to the ceiling",
            script: format!(
                "## @type: DataSource\n## @args: [nested = {open}{close}]\n## @return: \
                 datasource0\ndatasource0 = x()\n",
                open = "[".repeat(MAX_ANNOTATION_NESTING),
                close = "]".repeat(MAX_ANNOTATION_NESTING),
            ),
            nodes: vec![("datasource0", vec!["nested"])],
        },
    ];

    let router = emulator();
    for row in &served {
        let answered = ok(
            &router,
            "GetDataflowGraph",
            &json!({ "PythonScript": row.script }),
        )
        .await;
        let nodes = node_rows(row.case, &answered);
        let read: Vec<(String, Vec<String>)> = nodes
            .iter()
            .map(|(id, _, _, args)| {
                (
                    id.clone(),
                    args.iter().map(|(name, _, _)| name.clone()).collect(),
                )
            })
            .collect();
        let expected: Vec<(String, Vec<String>)> = row
            .nodes
            .iter()
            .map(|(id, args)| {
                (
                    id.to_string(),
                    args.iter().map(|name| name.to_string()).collect(),
                )
            })
            .collect();
        assert_eq!(
            read, expected,
            "case {case:?}: expected the nodes {expected:?}, got {read:?}",
            case = row.case,
        );
    }

    // One bracket past the nesting ceiling: refused, naming the depth and the ceiling.
    let deepest = MAX_ANNOTATION_NESTING + 1;
    assert_refusal(
        &router,
        "one bracket past the nesting ceiling",
        "GetDataflowGraph",
        &json!({
            "PythonScript": format!(
                "## @type: DataSource\n## @args: [nested = {open}{close}]\n## @return: \
                 datasource0\ndatasource0 = x()\n",
                open = "[".repeat(deepest),
                close = "]".repeat(deepest),
            ),
        }),
        "InvalidInputException",
        &format!(
            "the annotations of node datasource0 nest brackets {deepest} deep, more than the \
             {MAX_ANNOTATION_NESTING} levels one node's annotations may nest"
        ),
    )
    .await;

    // One byte past the size ceiling: refused as a request-size failure, before the script is
    // read at all.
    let oversized = "#".repeat(MAX_SCRIPT_BYTES + 1);
    assert_refusal(
        &router,
        "one byte past the script ceiling",
        "GetDataflowGraph",
        &json!({ "PythonScript": oversized }),
        "InvalidInputException",
        &format!(
            "the script carries {carried} bytes, more than the {MAX_SCRIPT_BYTES} bytes one \
             script may carry",
            carried = MAX_SCRIPT_BYTES + 1,
        ),
    )
    .await;

    // AT the size ceiling the script is still read: the bound admits its own ceiling.
    let node = "## @type: DataSource\n## @return: datasource0\ndatasource0 = x()\n";
    let padded = format!(
        "{node}{}",
        "#".repeat(MAX_SCRIPT_BYTES.saturating_sub(node.len()))
    );
    let answered = ok(
        &router,
        "GetDataflowGraph",
        &json!({ "PythonScript": padded }),
    )
    .await;
    assert_eq!(
        node_rows("at the script ceiling", &answered).len(),
        1,
        "a script at the size ceiling must still be read, got {answered}"
    );
}

/// A node annotated with AS MANY properties as one node of the answer may carry is served in
/// full, and one property past that is refused: the bound is `CodeGenNode$Args`'s own, a
/// list of length 0..50 (`smithy_query shape=CodeGenNode`).
#[tokio::test]
async fn the_argument_bound_admits_the_ceiling_and_refuses_past_it() {
    use service_com_amazonaws_glue_awsglue::ops::get_dataflow_graph::MAX_NODE_ARGS;

    /// A script whose one node is annotated with `count` properties.
    fn script_with(count: usize) -> String {
        let args: Vec<String> = (0..count)
            .map(|at| format!("arg{at} = \"value{at}\""))
            .collect();
        format!(
            "## @type: DataSource\n## @args: [{}]\n## @return: datasource0\ndatasource0 = x()\n",
            args.join(", ")
        )
    }

    let router = emulator();
    let answered = ok(
        &router,
        "GetDataflowGraph",
        &json!({ "PythonScript": script_with(MAX_NODE_ARGS) }),
    )
    .await;
    let nodes = node_rows("at the argument ceiling", &answered);
    assert_eq!(
        nodes[0].3.len(),
        MAX_NODE_ARGS,
        "a node at the argument ceiling must carry every argument, got {answered}"
    );

    let past = MAX_NODE_ARGS + 1;
    assert_refusal(
        &router,
        "one argument past the ceiling",
        "GetDataflowGraph",
        &json!({ "PythonScript": script_with(past) }),
        "InvalidInputException",
        &format!(
            "node datasource0 is annotated with {past} arguments, more than the {MAX_NODE_ARGS} \
             arguments one node may carry"
        ),
    )
    .await;
}
