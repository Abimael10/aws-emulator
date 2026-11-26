// SPDX-License-Identifier: CC0-1.0

//! The Neptune BULK-LOAD DATA FORMATS, read once for every service that loads a graph from
//! them.
//!
//! Two emulated services take a bulk load: the Neptune data plane's loader endpoint
//! (`neptunedata`, `StartLoaderJob`) and Neptune Analytics' import task
//! (`neptune-graph`, `StartImportTask`). They load into stores of their own and report
//! their own numbers and their own sentences, but the FORMATS are the same published ones,
//! so how a file spells an element is read here and not twice:
//!
//! * the two delimited formats — the Gremlin CSV load format, whose element columns are
//!   `~id`, `~label`, `~from` and `~to`, and the openCypher CSV load format, whose element
//!   columns are `:ID`, `:LABEL`, `:START_ID`, `:END_ID` and `:TYPE`, with every other
//!   column a property spelled `key:Type` in both ([`LoadColumns`], [`read_delimited`]);
//! * the RDF syntaxes a load's statements are written in ([`read_rdf`]), and the standard
//!   reading of a statement as a property-graph element: a triple whose object is a
//!   LITERAL sets a property on the subject's node keyed by the predicate, a triple whose
//!   object is a RESOURCE joins the two nodes with an edge labelled by the predicate, and
//!   `rdf:type` gives the subject a LABEL.
//!
//! The parsing is done by the maintained crates for each syntax (`csv` for the delimited
//! formats, `oxttl` for the RDF ones), so nothing here matches on source text. What a
//! delimited file holds is decided by its COLUMNS rather than by its name: a record that
//! names two endpoints is an edge and any other record is a node, which is how one prefix
//! can hold a load's vertex files and its edge files together.
//!
//! What each SERVICE keeps is the element model it loads into and the words it reports
//! with: the reader hands every element and every fault to the caller's own [`LoadSink`],
//! so the two services' ceilings, counters and messages stay their own. The reader itself
//! retains nothing — it holds one record at a time, and the sink's [`LoadSink::admit_record`]
//! decides whether that record is read at all, which is what keeps the memory one load
//! takes bounded by the sink's own ceiling rather than by the size of its source.

use ::serde_json::Value;

/// The character that separates the several values one cell may carry — a node's labels in
/// both delimited formats, and the values of an array-valued property column.
pub const MULTI_VALUE_SEPARATOR: char = ';';

/// The column names one delimited bulk-load format spells its elements with.
///
/// The two sets are the only difference between the delimited formats, so they are stated
/// here once and the record rules below are shared.
#[derive(Debug, Clone, Copy)]
pub struct LoadColumns {
    /// The column holding the element's own id.
    pub id: &'static str,
    /// The column holding a node's labels.
    pub label: &'static str,
    /// The column holding the id of the node an edge leaves.
    pub from: &'static str,
    /// The column holding the id of the node an edge enters.
    pub to: &'static str,
    /// The column holding an edge's label.
    pub edge_label: &'static str,
}

impl LoadColumns {
    /// The Gremlin CSV load format's columns, which Neptune's columnar (Parquet) load
    /// format spells identically.
    pub const GREMLIN: Self = Self {
        id: "~id",
        label: "~label",
        from: "~from",
        to: "~to",
        // The Gremlin format labels a node and an edge with the SAME column.
        edge_label: "~label",
    };

    /// The openCypher CSV load format's columns.
    pub const OPEN_CYPHER: Self = Self {
        id: ":ID",
        label: ":LABEL",
        from: ":START_ID",
        to: ":END_ID",
        edge_label: ":TYPE",
    };
}

/// How one load reads its delimited objects: the columns its format spells elements with,
/// and the rules a caller's own API settles.
#[derive(Debug, Clone, Copy)]
pub struct ReadOptions {
    /// The element columns the format names.
    pub columns: LoadColumns,
    /// Whether a file whose header declares no id column names no element at all. A format
    /// whose relationship files may leave the id column out sets this `false`, and the
    /// records of such a file are then read against `edge_ids_from_data`.
    pub header_requires_id: bool,
    /// Whether an edge record must name its own id.
    pub edge_ids_from_data: bool,
    /// Whether an empty cell is a value rather than nothing.
    pub allow_empty_strings: bool,
    /// Whether a record carrying a value that does not read as its column's declared type
    /// is SKIPPED, or is loaded without that value. Each service's own API says which:
    /// "skipping any entries with errors" skips the record, while "skips the data that
    /// caused the error and continues if possible" keeps the record and loses the value.
    pub skip_record_on_fault: bool,
    /// The most columns one file may declare.
    pub max_columns: usize,
    /// The most characters one value the load reads keeps.
    pub value_chars: usize,
}

/// Why a record, or a whole file, could not be read.
///
/// A fault carries what was wrong and nothing about how to SAY it: each service words its
/// own API's sentence from this ([`LoadSink::note_fault`]), and each decides whether the
/// fault stops the load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFault {
    /// The file could not be read in the format the load named.
    FileUnreadable,
    /// The file declares more columns than the load reads in one.
    TooManyColumns { ceiling: usize },
    /// The file's header declares no id column, so none of its records names an element.
    HeaderWithoutId { column: &'static str },
    /// The record could not be read in the format the load named.
    RecordUnreadable,
    /// The record names no element id.
    NoElementId,
    /// The record is an edge carrying no label.
    EdgeWithoutLabel,
    /// The record is an edge naming no id, and the load's edge ids come from its data.
    EdgeWithoutId,
    /// The record names one endpoint of an edge, not both.
    OneEndpointOnly,
    /// A value does not read as the type its column declared.
    UndeclaredType { key: String, declared: String },
}

/// What a bulk-load reader hands its caller: the elements it read, and what it could not
/// read.
///
/// The service on the other side owns the element model, the counters, the ceilings and the
/// wording; the reader owns the formats.
pub trait LoadSink {
    /// Admit one more record of the source, or stop the load. `false` is the reader's cue
    /// to read no further, which is also how a sink that has already failed stops a read.
    fn admit_record(&mut self) -> bool;

    /// Take one node: its id, the labels it carries and the properties it sets.
    fn take_node(
        &mut self,
        id: String,
        labels: Vec<String>,
        properties: Vec<(String, Value)>,
        record_num: u64,
    );

    /// Take one edge: its id where the data named one, its label, the nodes it joins and
    /// the properties it sets.
    fn take_edge(
        &mut self,
        id: Option<String>,
        label: String,
        from: String,
        to: String,
        properties: Vec<(String, Value)>,
        record_num: u64,
    );

    /// Record that something could not be read. `record_num` is `0` for a fault that
    /// belongs to the file as a whole.
    fn note_fault(&mut self, fault: &LoadFault, record_num: u64);

    /// Which object of the source is being read, counted from one. It scopes the ids a
    /// document-local name converts to ([`blank_node_iri`]), because two objects of one
    /// load name their own local names independently.
    fn document(&self) -> usize {
        1
    }
}

/// One file's header, resolved to the positions the loader reads each column at.
#[derive(Debug, Default)]
pub struct Header {
    pub id: Option<usize>,
    pub label: Option<usize>,
    pub from: Option<usize>,
    pub to: Option<usize>,
    pub edge_label: Option<usize>,
    /// Every other column: where it sits, the property key it sets, the type the header
    /// declared for it if any, and whether the column is array-valued.
    pub properties: Vec<PropertyColumn>,
}

/// One property column of a delimited load file.
#[derive(Debug, Clone)]
pub struct PropertyColumn {
    /// Where the column sits in a record.
    pub at: usize,
    /// The property key it sets.
    pub key: String,
    /// The type the header declared for it, if any.
    pub declared: Option<String>,
    /// Whether the column carries SEVERAL values, which both delimited formats spell by
    /// suffixing the declared type with `[]`.
    pub array: bool,
}

/// `text` as a load stores it: no longer than `max_chars`, cut on a character boundary.
///
/// Applied where text ENTERS a reading, so nothing a load retains is as long as the file it
/// came from.
#[must_use]
pub fn clamped(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Resolve one file's header to the positions each column sits at.
#[must_use]
pub fn resolve_header(options: &ReadOptions, header: &::csv::StringRecord) -> Header {
    let columns = options.columns;
    let mut resolved = Header::default();
    for (index, raw) in header.iter().enumerate() {
        let name = raw.trim();
        if name.eq_ignore_ascii_case(columns.id) {
            resolved.id = Some(index);
        } else if name.eq_ignore_ascii_case(columns.label) {
            resolved.label = Some(index);
        } else if name.eq_ignore_ascii_case(columns.from) {
            resolved.from = Some(index);
        } else if name.eq_ignore_ascii_case(columns.to) {
            resolved.to = Some(index);
        } else if name.eq_ignore_ascii_case(columns.edge_label) {
            resolved.edge_label = Some(index);
        } else {
            // Every other column is a property, spelled `key` or `key:Type`; both
            // delimited formats spell an array-valued column by suffixing the type with
            // `[]`, and the Gremlin format may state a cardinality in brackets after it.
            // Both halves are a header cell the caller sized, so both are clamped where
            // they enter.
            let (key, declared) = match name.split_once(':') {
                Some((key, declared)) => (key.trim(), Some(declared.trim())),
                None => (name, None),
            };
            if key.is_empty() {
                continue;
            }
            let array = declared.is_some_and(|declared| declared.ends_with("[]"));
            let declared = declared.map(|declared| {
                let bare = declared.trim_end_matches("[]");
                let bare = bare.split_once('(').map_or(bare, |(name, _)| name);
                clamped(bare.trim(), options.value_chars)
            });
            resolved.properties.push(PropertyColumn {
                at: index,
                key: clamped(key, options.value_chars),
                declared,
                array,
            });
        }
    }
    resolved
}

/// Read one delimited bulk-load file (the Gremlin CSV or the openCypher CSV format) into
/// `sink`.
///
/// One record is held at a time: the reader streams the file, and the sink decides whether
/// each record is read before anything of it is retained.
pub fn read_delimited<S: LoadSink>(sink: &mut S, options: &ReadOptions, object: &[u8]) {
    let mut reader = ::csv::ReaderBuilder::new()
        // A load file may leave the trailing property columns of a record out.
        .flexible(true)
        .trim(::csv::Trim::All)
        .from_reader(object);
    let header = match reader.headers() {
        Ok(header) if header.len() > options.max_columns => {
            sink.note_fault(
                &LoadFault::TooManyColumns {
                    ceiling: options.max_columns,
                },
                0,
            );
            return;
        }
        Ok(header) => resolve_header(options, header),
        Err(_) => {
            sink.note_fault(&LoadFault::FileUnreadable, 0);
            return;
        }
    };
    if header.id.is_none() && (options.header_requires_id || header.from.is_none()) {
        sink.note_fault(
            &LoadFault::HeaderWithoutId {
                column: options.columns.id,
            },
            0,
        );
        return;
    }
    for (index, record) in reader.records().enumerate() {
        if !sink.admit_record() {
            return;
        }
        // The record number a caller counts from is the record's own position in the file,
        // after its header.
        let record_num = (index as u64).saturating_add(2);
        match record {
            Ok(record) => read_record(sink, options, &header, &record, record_num),
            Err(_) => sink.note_fault(&LoadFault::RecordUnreadable, record_num),
        }
    }
}

/// Read one record of a load file against `header`.
///
/// Public because the COLUMNAR load format spells the same columns and yields the same
/// records: a caller that decodes those rows reads each one through here, so one set of
/// record rules serves every format that carries them.
pub fn read_record<S: LoadSink>(
    sink: &mut S,
    options: &ReadOptions,
    header: &Header,
    record: &::csv::StringRecord,
    record_num: u64,
) {
    let cell = |at: Option<usize>| {
        at.and_then(|index| record.get(index))
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    // What the record NAMES is read first, so a record that names no element at all is one
    // fault rather than one per cell it also got wrong.
    let from = cell(header.from).map(|value| clamped(value, options.value_chars));
    let to = cell(header.to).map(|value| clamped(value, options.value_chars));
    let id = cell(header.id).map(|value| clamped(value, options.value_chars));
    let named = match (from, to) {
        (Some(from), Some(to)) => {
            let Some(label) = cell(header.edge_label).or_else(|| cell(header.label)) else {
                sink.note_fault(&LoadFault::EdgeWithoutLabel, record_num);
                return;
            };
            if id.is_none() && options.edge_ids_from_data {
                sink.note_fault(&LoadFault::EdgeWithoutId, record_num);
                return;
            }
            Named::Edge {
                id,
                label: clamped(label, options.value_chars),
                from,
                to,
            }
        }
        (None, None) => {
            let Some(id) = id else {
                sink.note_fault(&LoadFault::NoElementId, record_num);
                return;
            };
            // A label cell carries as many labels as the caller separated; what one node
            // may hold is the sink's own ceiling.
            let labels: Vec<String> = cell(header.label)
                .map(|named| {
                    named
                        .split(MULTI_VALUE_SEPARATOR)
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .map(|label| clamped(label, options.value_chars))
                        .collect()
                })
                .unwrap_or_default();
            Named::Node { id, labels }
        }
        _ => {
            sink.note_fault(&LoadFault::OneEndpointOnly, record_num);
            return;
        }
    };
    let mut properties: Vec<(String, Value)> = Vec::new();
    let mut faults: Vec<LoadFault> = Vec::new();
    for column in &header.properties {
        let raw = record.get(column.at).map(str::trim).unwrap_or_default();
        // An empty cell is a null unless the load said an empty string is a value.
        if raw.is_empty() && !options.allow_empty_strings {
            continue;
        }
        let read = if column.array {
            typed_values(column.declared.as_deref(), raw, options.value_chars)
        } else {
            typed_value(column.declared.as_deref(), raw, options.value_chars)
        };
        match read {
            Ok(value) => properties.push((column.key.clone(), value)),
            Err(declared) => faults.push(LoadFault::UndeclaredType {
                key: column.key.clone(),
                declared,
            }),
        }
    }
    for fault in &faults {
        sink.note_fault(fault, record_num);
    }
    // A record carrying a value that does not read as its column's declared type is an
    // entry with an error: whether the load skips that entry or keeps it without the value
    // is what each service's own documentation settles.
    if !faults.is_empty() && options.skip_record_on_fault {
        return;
    }
    match named {
        Named::Node { id, labels } => sink.take_node(id, labels, properties, record_num),
        Named::Edge {
            id,
            label,
            from,
            to,
        } => sink.take_edge(id, label, from, to, properties, record_num),
    }
}

/// What one record NAMES: a node, or an edge between two of them.
enum Named {
    Node {
        id: String,
        labels: Vec<String>,
    },
    Edge {
        id: Option<String>,
        label: String,
        from: String,
        to: String,
    },
}

/// One cell as the JSON value the property it sets keeps, from the type its column
/// declared, or the declared type it does not read as.
///
/// Both delimited load formats declare a property column as `key:Type`, and the type names
/// are the ones those formats list (`Bool`/`Boolean`, `Byte`, `Short`, `Int`/`Integer`,
/// `Long`, `Float`, `Double`, `String`, `Date`/`DateTime`). A column declaring a type this
/// reading does not narrow keeps the cell's text, which every one of those types admits.
pub fn typed_value(
    declared: Option<&str>,
    raw: &str,
    value_chars: usize,
) -> Result<Value, String> {
    let Some(declared) = declared.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(Value::String(clamped(raw, value_chars)));
    };
    let mismatch = || declared.to_string();
    match declared.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => raw.parse::<bool>().map(Value::Bool).map_err(|_| mismatch()),
        "byte" | "short" | "int" | "integer" | "long" => {
            raw.parse::<i64>().map(Value::from).map_err(|_| mismatch())
        }
        "float" | "double" => raw
            .parse::<f64>()
            .ok()
            .and_then(::serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(mismatch),
        _ => Ok(Value::String(clamped(raw, value_chars))),
    }
}

/// An array-valued cell as the JSON array the property it sets keeps: the cell's values,
/// each read as the type the column declared.
pub fn typed_values(
    declared: Option<&str>,
    raw: &str,
    value_chars: usize,
) -> Result<Value, String> {
    let mut values: Vec<Value> = Vec::new();
    for value in raw
        .split(MULTI_VALUE_SEPARATOR)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        values.push(typed_value(declared, value, value_chars)?);
    }
    Ok(Value::Array(values))
}

/// The RDF syntax one load file is written in.
///
/// The syntaxes the maintained line-based and Turtle parser reads (`oxttl`); RDF/XML is not
/// among them, so a service whose model names it answers for that format itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdfSyntax {
    NTriples,
    NQuads,
    Turtle,
}

/// One statement a reading yields: its subject, its predicate and its object.
type Statement = (::oxrdf::NamedOrBlankNode, ::oxrdf::NamedNode, ::oxrdf::Term);

/// Read one RDF bulk-load file into `sink`.
///
/// Every syntax is parsed by the maintained crate for it and every statement is taken by
/// the one reading below, so no RDF format loads the same statement differently. A relative
/// IRI resolves against `base_iri`, which is what a load's parser configuration names; a
/// base the parser will not take is dropped, and a relative IRI in the file is then a fault
/// of its own statement.
///
/// The parser is pulled ONE statement at a time and the sink decides whether that statement
/// is read before any of it is retained, so what a load holds is bounded by the sink's own
/// ceiling and not by the number of statements the file carries.
pub fn read_rdf<S: LoadSink>(
    sink: &mut S,
    syntax: RdfSyntax,
    base_iri: &str,
    object: &[u8],
    value_chars: usize,
) {
    let statements: Box<dyn Iterator<Item = Result<Statement, ()>> + '_> = match syntax {
        RdfSyntax::NTriples => Box::new(::oxttl::NTriplesParser::new().for_reader(object).map(
            |parsed| {
                parsed
                    .map(|triple| (triple.subject, triple.predicate, triple.object))
                    .map_err(|_| ())
            },
        )),
        RdfSyntax::NQuads => Box::new(::oxttl::NQuadsParser::new().for_reader(object).map(
            |parsed| {
                parsed
                    .map(|quad| (quad.subject, quad.predicate, quad.object))
                    .map_err(|_| ())
            },
        )),
        RdfSyntax::Turtle => {
            let parser = ::oxttl::TurtleParser::new()
                .with_base_iri(base_iri)
                .unwrap_or_else(|_| ::oxttl::TurtleParser::new());
            Box::new(parser.for_reader(object).map(|parsed| {
                parsed
                    .map(|triple| (triple.subject, triple.predicate, triple.object))
                    .map_err(|_| ())
            }))
        }
    };
    let document = sink.document();
    for (index, statement) in statements.enumerate() {
        if !sink.admit_record() {
            return;
        }
        let record_num = (index as u64).saturating_add(1);
        match statement {
            Ok((subject, predicate, object)) => take_statement(
                sink,
                document,
                &subject,
                &predicate,
                &object,
                record_num,
                value_chars,
            ),
            Err(()) => sink.note_fault(&LoadFault::RecordUnreadable, record_num),
        }
    }
}

/// Take one RDF statement as the property-graph element it loads.
///
/// Public because a service may read a syntax this module does not parse and still has to
/// load the statements it yields the same way.
pub fn take_statement<S: LoadSink>(
    sink: &mut S,
    document: usize,
    subject: &::oxrdf::NamedOrBlankNode,
    predicate: &::oxrdf::NamedNode,
    object: &::oxrdf::Term,
    record_num: u64,
    value_chars: usize,
) {
    use ::oxrdf::{NamedOrBlankNode, Term};

    let subject = match subject {
        NamedOrBlankNode::NamedNode(node) => clamped(node.as_str(), value_chars),
        NamedOrBlankNode::BlankNode(node) => {
            clamped(&blank_node_iri(document, node.as_str()), value_chars)
        }
    };
    let predicate_iri = clamped(predicate.as_str(), value_chars);
    match object {
        // A literal is a PROPERTY of the subject, keyed by the predicate.
        Term::Literal(literal) => sink.take_node(
            subject,
            Vec::new(),
            vec![(predicate_iri, literal_value(literal, value_chars))],
            record_num,
        ),
        // `rdf:type` gives the subject a LABEL.
        Term::NamedNode(node) if predicate == &::oxrdf::vocab::rdf::TYPE => sink.take_node(
            subject,
            vec![clamped(node.as_str(), value_chars)],
            Vec::new(),
            record_num,
        ),
        // Any other resource JOINS the two nodes, and the statement itself is the id of the
        // edge it loads, so the same statement read twice loads one edge.
        Term::NamedNode(_) | Term::BlankNode(_) => {
            let target = match object {
                Term::NamedNode(node) => clamped(node.as_str(), value_chars),
                Term::BlankNode(node) => {
                    clamped(&blank_node_iri(document, node.as_str()), value_chars)
                }
                Term::Literal(_) => return,
            };
            // Both endpoints are nodes of this load: a resource a statement names is a node
            // of the graph the load writes.
            sink.take_node(subject.clone(), Vec::new(), Vec::new(), record_num);
            sink.take_node(target.clone(), Vec::new(), Vec::new(), record_num);
            let id = statement_edge_id(&subject, &predicate_iri, &target);
            sink.take_edge(
                Some(clamped(&id, value_chars)),
                predicate_iri,
                subject,
                target,
                Vec::new(),
                record_num,
            );
        }
    }
}

/// The IRI a blank node labelled `label` in the `document`-th file of a source is loaded
/// under.
///
/// A blank node's label is local to the document it is written in — "blank nodes are
/// converted to unique IRIs at load time" — so the IRI it is loaded under is that label
/// scoped to the document, which makes it unique across the whole load.
#[must_use]
pub fn blank_node_iri(document: usize, label: &str) -> String {
    format!("urn:bnode:{document}:{label}")
}

/// The id of the edge one resource statement loads: the statement itself, so the same
/// statement read twice loads one edge — RDF statements are a set.
#[must_use]
pub fn statement_edge_id(subject: &str, predicate: &str, object: &str) -> String {
    format!("<{subject}> <{predicate}> <{object}>")
}

/// One RDF literal as the JSON value the property it sets keeps.
///
/// The XSD datatype the literal carries is what decides the type — a number stays a number
/// and a boolean stays a boolean — and everything else keeps its lexical form, which is the
/// only reading of it the datatype allows.
#[must_use]
pub fn literal_value(literal: &::oxrdf::Literal, value_chars: usize) -> Value {
    use ::oxrdf::vocab::xsd;

    let text = literal.value();
    let datatype = literal.datatype();
    if datatype == xsd::BOOLEAN {
        if let Ok(parsed) = text.parse::<bool>() {
            return Value::Bool(parsed);
        }
    }
    if datatype == xsd::INTEGER
        || datatype == xsd::INT
        || datatype == xsd::LONG
        || datatype == xsd::SHORT
        || datatype == xsd::BYTE
    {
        if let Ok(parsed) = text.parse::<i64>() {
            return Value::from(parsed);
        }
    }
    if datatype == xsd::DOUBLE || datatype == xsd::FLOAT || datatype == xsd::DECIMAL {
        if let Some(parsed) = text
            .parse::<f64>()
            .ok()
            .and_then(::serde_json::Number::from_f64)
        {
            return Value::Number(parsed);
        }
    }
    Value::String(clamped(text, value_chars))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One node a reading handed the sink: its id, its labels and its properties.
    type RecordedNode = (String, Vec<String>, Vec<(String, Value)>);

    /// One edge a reading handed the sink: its id, its label and the nodes it joins.
    type RecordedEdge = (Option<String>, String, String, String);

    /// A sink that records what a reading handed it, so the format rules are tested without
    /// a graph store.
    #[derive(Debug, Default)]
    struct Recorded {
        nodes: Vec<RecordedNode>,
        edges: Vec<RecordedEdge>,
        faults: Vec<(LoadFault, u64)>,
        /// How many records the sink admits before it stops.
        budget: usize,
        admitted: usize,
    }

    impl Recorded {
        fn with_budget(budget: usize) -> Self {
            Self {
                budget,
                ..Self::default()
            }
        }
    }

    impl LoadSink for Recorded {
        fn admit_record(&mut self) -> bool {
            if self.admitted >= self.budget {
                return false;
            }
            self.admitted += 1;
            true
        }

        fn take_node(
            &mut self,
            id: String,
            labels: Vec<String>,
            properties: Vec<(String, Value)>,
            _record_num: u64,
        ) {
            self.nodes.push((id, labels, properties));
        }

        fn take_edge(
            &mut self,
            id: Option<String>,
            label: String,
            from: String,
            to: String,
            _properties: Vec<(String, Value)>,
            _record_num: u64,
        ) {
            self.edges.push((id, label, from, to));
        }

        fn note_fault(&mut self, fault: &LoadFault, record_num: u64) {
            self.faults.push((fault.clone(), record_num));
        }
    }

    /// The options both delimited formats are read with in these tests.
    fn options(columns: LoadColumns) -> ReadOptions {
        ReadOptions {
            columns,
            header_requires_id: false,
            edge_ids_from_data: true,
            allow_empty_strings: false,
            skip_record_on_fault: true,
            max_columns: 64,
            value_chars: 4096,
        }
    }

    /// RULE: a delimited load file's COLUMNS decide what each record holds — a record
    /// naming both endpoints is an edge, any other record is a node, and a property column
    /// is read as the type its header declared.
    ///
    /// The rows span both formats' column vocabularies, node and edge records, several
    /// labels in one cell, each narrowed type, an array-valued column, and the records that
    /// name no element.
    #[test]
    fn the_columns_decide_what_a_record_holds() {
        let gremlin = "~id,~label,name:String,age:Int,ok:Bool,size:Double,tags:String[]\n\
                       v1,Person;Employee,Alice,30,true,1.5,a;b\n\
                       v2,Person,Bob,41,false,2.5,\n";
        let mut recorded = Recorded::with_budget(16);
        read_delimited(
            &mut recorded,
            &options(LoadColumns::GREMLIN),
            gremlin.as_bytes(),
        );
        assert_eq!(
            recorded.nodes.len(),
            2,
            "case \"the Gremlin CSV format\": expected two nodes, got {:?}",
            recorded.nodes
        );
        let (id, labels, properties) = &recorded.nodes[0];
        assert_eq!(
            (id.as_str(), labels.as_slice()),
            ("v1", ["Person".to_string(), "Employee".to_string()].as_slice()),
            "case \"a multi-labelled node\": got {:?}",
            recorded.nodes[0]
        );
        assert_eq!(
            properties.as_slice(),
            [
                ("name".to_string(), Value::from("Alice")),
                ("age".to_string(), Value::from(30)),
                ("ok".to_string(), Value::Bool(true)),
                ("size".to_string(), Value::from(1.5)),
                (
                    "tags".to_string(),
                    Value::Array(vec![Value::from("a"), Value::from("b")])
                ),
            ]
            .as_slice(),
            "case \"the declared types\": got {properties:?}"
        );

        let open_cypher = ":ID,:START_ID,:END_ID,:TYPE\ne1,v1,v2,KNOWS\n";
        let mut recorded = Recorded::with_budget(16);
        read_delimited(
            &mut recorded,
            &options(LoadColumns::OPEN_CYPHER),
            open_cypher.as_bytes(),
        );
        assert_eq!(
            recorded.edges.as_slice(),
            [(
                Some("e1".to_string()),
                "KNOWS".to_string(),
                "v1".to_string(),
                "v2".to_string()
            )]
            .as_slice(),
            "case \"the openCypher CSV format\": got {:?}",
            recorded.edges
        );

        // The records that name no element, each reported as its own fault.
        let faulty = "~id,~label,~from,~to,age:Int\n\
                      ,Person,,,30\n\
                      v3,,v1,,\n\
                      v4,,v1,v2,\n\
                      v5,Person,,,oops\n";
        let mut recorded = Recorded::with_budget(16);
        read_delimited(
            &mut recorded,
            &options(LoadColumns::GREMLIN),
            faulty.as_bytes(),
        );
        let faults: Vec<LoadFault> = recorded
            .faults
            .iter()
            .map(|(fault, _)| fault.clone())
            .collect();
        assert_eq!(
            faults.as_slice(),
            [
                LoadFault::NoElementId,
                LoadFault::OneEndpointOnly,
                LoadFault::EdgeWithoutLabel,
                LoadFault::UndeclaredType {
                    key: "age".to_string(),
                    declared: "Int".to_string()
                },
            ]
            .as_slice(),
            "case \"the records that name no element\": got {faults:?}"
        );
    }

    /// RULE: the sink's own ceiling decides how many records a reading reads, so a file
    /// holding more records than the load admits is read up to that ceiling and no further.
    #[test]
    fn the_sink_decides_how_many_records_are_read() {
        let mut rows = String::from("~id,~label\n");
        for index in 0..50 {
            rows.push_str(&format!("v{index},Person\n"));
        }
        let mut recorded = Recorded::with_budget(7);
        read_delimited(&mut recorded, &options(LoadColumns::GREMLIN), rows.as_bytes());
        assert_eq!(
            recorded.nodes.len(),
            7,
            "case \"a file past the sink's ceiling\": expected the ceiling's worth of \
             records, got {}",
            recorded.nodes.len()
        );
    }

    /// RULE: an RDF statement loads the standard property-graph element — a literal is a
    /// property, `rdf:type` is a label, and any other resource is an edge whose id is the
    /// statement.
    ///
    /// The rows span the three syntaxes `oxttl` reads, each kind of object, a typed literal
    /// and a blank node.
    #[test]
    fn a_statement_loads_the_element_the_standard_reading_gives_it() {
        let cases: [(&str, RdfSyntax, &str); 3] = [
            (
                "N-Triples",
                RdfSyntax::NTriples,
                "<http://e/s1> <http://e/name> \"Erin\" .\n\
                 <http://e/s1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://e/P> .\n\
                 <http://e/s1> <http://e/knows> <http://e/s2> .\n\
                 <http://e/s1> <http://e/age> \"30\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            ),
            (
                "N-Quads",
                RdfSyntax::NQuads,
                "<http://e/s1> <http://e/name> \"Erin\" <http://e/g> .\n\
                 <http://e/s1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://e/P> <http://e/g> .\n\
                 <http://e/s1> <http://e/knows> <http://e/s2> <http://e/g> .\n\
                 <http://e/s1> <http://e/age> \"30\"^^<http://www.w3.org/2001/XMLSchema#integer> <http://e/g> .\n",
            ),
            (
                "Turtle",
                RdfSyntax::Turtle,
                "@prefix e: <http://e/> .\n\
                 @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
                 e:s1 e:name \"Erin\" ; a e:P ; e:knows e:s2 ; e:age \"30\"^^xsd:integer .\n",
            ),
        ];
        for (case, syntax, document) in cases {
            let mut recorded = Recorded::with_budget(32);
            read_rdf(&mut recorded, syntax, "http://e/", document.as_bytes(), 4096);
            assert!(
                recorded.faults.is_empty(),
                "case {case:?}: expected every statement to read, got {:?}",
                recorded.faults
            );
            assert_eq!(
                recorded.edges.as_slice(),
                [(
                    Some("<http://e/s1> <http://e/knows> <http://e/s2>".to_string()),
                    "http://e/knows".to_string(),
                    "http://e/s1".to_string(),
                    "http://e/s2".to_string()
                )]
                .as_slice(),
                "case {case:?}: expected the resource statement to load one edge, got {:?}",
                recorded.edges
            );
            let labelled = recorded
                .nodes
                .iter()
                .any(|(id, labels, _)| id == "http://e/s1" && labels == &["http://e/P"]);
            assert!(
                labelled,
                "case {case:?}: expected rdf:type to label the subject, got {:?}",
                recorded.nodes
            );
            let named = recorded.nodes.iter().any(|(id, _, properties)| {
                id == "http://e/s1"
                    && properties.contains(&("http://e/name".to_string(), Value::from("Erin")))
            });
            assert!(
                named,
                "case {case:?}: expected the literal to set a property, got {:?}",
                recorded.nodes
            );
            let typed = recorded.nodes.iter().any(|(_, _, properties)| {
                properties.contains(&("http://e/age".to_string(), Value::from(30)))
            });
            assert!(
                typed,
                "case {case:?}: expected the typed literal to keep its type, got {:?}",
                recorded.nodes
            );
        }

        // A blank node is loaded under an IRI scoped to the document it was written in.
        let mut recorded = Recorded::with_budget(8);
        read_rdf(
            &mut recorded,
            RdfSyntax::NTriples,
            "http://e/",
            b"_:b0 <http://e/name> \"Anon\" .\n",
            4096,
        );
        assert_eq!(
            recorded.nodes.first().map(|(id, _, _)| id.as_str()),
            Some(blank_node_iri(1, "b0").as_str()),
            "case \"a blank node\": got {:?}",
            recorded.nodes
        );

        // A statement no syntax can read is a fault of its own record, and the rest of the
        // file is still read.
        let mut recorded = Recorded::with_budget(8);
        read_rdf(
            &mut recorded,
            RdfSyntax::NTriples,
            "http://e/",
            b"this is not a statement\n",
            4096,
        );
        assert_eq!(
            recorded.faults.first().map(|(fault, _)| fault.clone()),
            Some(LoadFault::RecordUnreadable),
            "case \"an unreadable statement\": got {:?}",
            recorded.faults
        );
    }
}
