// SPDX-License-Identifier: CC0-1.0

//! What ONE load reads, and the elements each bulk-load format yields it.
//!
//! The formats themselves are read by the tree's one reader for them
//! (`::service_common::graph_bulk_load`), which this service shares with Neptune
//! Analytics' import task: the delimited load formats (the Gremlin CSV and openCypher CSV
//! column vocabularies), and the RDF syntaxes a load's statements are written in. This file
//! is what the loader of THIS service does with what that reader yields — the elements it
//! collects for the graph, the counts `GetLoaderJobStatus` reports, the sentences the job's
//! error log carries, and the ceilings one load is bounded by.
//!
//! The one format this service's model names that the shared reader does not parse is
//! RDF/XML ([`reads_format`]): every maintained parser for it is built on an XML reader
//! whose released versions this project's dependency audit refuses, so no load of that
//! format is performed — and the operation says so rather than reporting a load nothing
//! ran.

use super::*;

/// The most records — rows or statements — one load reads out of its source.
///
/// Emulator resource guard, NOT a quota AWS publishes: the byte ceilings the objects are
/// fetched under bound the ENCODED source, so this is what bounds the per-record work one
/// load performs.
pub const MAX_LOAD_RECORDS: u64 = 100_000;

/// The most characters one value a load stores keeps: an element id, a label, a property
/// key or a property value. Emulator resource guard, for the same reason.
pub const MAX_LOAD_VALUE_CHARS: usize = 4096;

/// The most columns one delimited load object may declare. Emulator resource guard: every
/// record is walked over the whole column list, so an object declaring more asks for
/// per-record work no element could keep.
pub const MAX_LOAD_COLUMNS: usize = 256;

/// The most bytes of element data one load materialises. Emulator resource guard, and the
/// companion of [`MAX_LOAD_RECORDS`]: the record count bounds how many values a load reads
/// and [`MAX_LOAD_VALUE_CHARS`] bounds each one, and this bounds their product.
pub const MAX_LOAD_DECODED_BYTES: usize = 16 * 1024 * 1024;

/// The most error-log entries one job keeps.
///
/// Emulator resource guard, well under the "last 10,000 error details per job" Neptune
/// documents: the log is kept for as long as the job is, so what one load can leave behind
/// is bounded.
pub const MAX_LOAD_ERROR_LOGS: usize = 1000;

/// The Gremlin CSV load format: "csv for the Gremlin CSV data format".
pub const FORMAT_CSV: &str = "csv";

/// The openCypher CSV load format.
pub const FORMAT_OPEN_CYPHER: &str = "opencypher";

/// The RDF N-Triples load format.
pub const FORMAT_NTRIPLES: &str = "ntriples";

/// The RDF N-Quads load format.
pub const FORMAT_NQUADS: &str = "nquads";

/// The RDF Turtle load format.
pub const FORMAT_TURTLE: &str = "turtle";

/// The RDF/XML load format.
pub const FORMAT_RDFXML: &str = "rdfxml";

/// Whether this loader READS the format `format` names.
///
/// Every modelled `Format` value but RDF/XML is read (`smithy_query(query="shape",
/// shape="Format")` -> `csv | nquads | ntriples | opencypher | rdfxml | turtle`). RDF/XML
/// is not: the maintained parsers for that syntax are built on an XML reader whose released
/// versions carry an advisory this project's dependency audit blocks, so nothing here can
/// read such a file — and a load id for a load nothing performed would be a fabrication.
#[must_use]
pub fn reads_format(format: &str) -> bool {
    format != FORMAT_RDFXML
}

/// One element a load read, before the graph takes it.
#[derive(Debug, Clone)]
pub enum LoadedElement {
    /// A vertex: its id, the labels it carries and the properties it sets.
    Node {
        id: String,
        labels: Vec<String>,
        properties: BTreeMap<String, Value>,
        file_name: String,
        record_num: u64,
    },
    /// An edge: its id where the data named one, its type, the vertices it joins and the
    /// properties it sets.
    Edge {
        id: Option<String>,
        rel_type: String,
        from: String,
        to: String,
        properties: BTreeMap<String, Value>,
        file_name: String,
        record_num: u64,
    },
}

/// What reading one load's source established, before any of it is written.
#[derive(Debug, Default)]
pub struct LoadReading {
    /// The elements the load read, in the order it read them.
    pub elements: Vec<LoadedElement>,
    /// How many records the load read.
    pub records: u64,
    /// How many records could not be read as the format the request named.
    pub parsing_errors: u64,
    /// How many values did not read as the type their column declared.
    pub datatype_mismatch_errors: u64,
    /// What each error was.
    pub error_logs: Vec<LoadErrorLog>,
    /// The load status a load that could not be carried out AT ALL stopped with: nothing
    /// it read is written, because there was nothing to read.
    pub halted: Option<&'static str>,
    /// Whether the load stopped before reading everything its source held, so it did not
    /// finish whatever its error flag says — which is what keeps it from reporting that it
    /// completed (see [`super::ComAmazonawsNeptunedataAmazonneptunedataplaneState::admit_load`]).
    pub incomplete: bool,
}

impl LoadReading {
    /// The reading of a load that could not be carried out at all, reported as `status` and
    /// explained by `log`.
    pub fn halted(status: &'static str, log: LoadErrorLog) -> Self {
        Self {
            halted: Some(status),
            error_logs: vec![log],
            ..Self::default()
        }
    }

    /// The reading of a load that read nothing because a previous load of the same source
    /// already took it, which is what `AUTO` and `RESUME` do.
    pub fn nothing_to_reload() -> Self {
        Self::default()
    }
}

/// The elements read so far, and what could not be read.
///
/// This is the [`::service_common::graph_bulk_load::LoadSink`] the shared format reader
/// hands every element and every fault to.
#[derive(Debug)]
pub struct LoadBuilder {
    reading: LoadReading,
    /// Whether the load stops at the first error it meets.
    fail_on_error: bool,
    /// How many bytes of element data the load has materialised.
    decoded: usize,
    /// The object being read, as the `s3://` location it was read from.
    file_name: String,
    /// Which object of the source is being read, counted from one.
    document: usize,
    /// Whether the load has stopped reading.
    stopped: bool,
}

impl LoadBuilder {
    /// A builder for the load `request` asks for.
    pub fn new(request: &LoadRequest) -> Self {
        Self {
            reading: LoadReading::default(),
            fail_on_error: request.fail_on_error,
            decoded: 0,
            file_name: String::new(),
            document: 0,
            stopped: false,
        }
    }

    /// Begin reading the object at `file_name`.
    pub fn begin_object(&mut self, file_name: impl Into<String>) {
        self.file_name = file_name.into();
        self.document = self.document.saturating_add(1);
    }

    /// Whether the load has stopped, which is every caller's cue to read no further.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// The reading this builder holds.
    pub fn finish(self) -> LoadReading {
        self.reading
    }

    /// Record that one object of the source is larger than this emulator reads in one load,
    /// which stops the load where it is: the ceiling is this emulator's own, so the
    /// sentence states it and quotes nothing out of the data.
    pub fn object_too_large(&mut self, ceiling: usize) {
        self.log(
            S3_READ_ERROR_CODE,
            format!(
                "The object is larger than the {ceiling} bytes this emulator reads in one \
                 object of a load."
            ),
            0,
        );
        self.stopped = true;
        self.reading.incomplete = true;
    }

    /// Record that one object of the source could not be fetched at all, so the load did
    /// not read everything the location held.
    pub fn object_unreadable(&mut self) {
        self.log(
            S3_READ_ERROR_CODE,
            "The object could not be read in Amazon S3.".to_string(),
            0,
        );
        self.reading.incomplete = true;
    }

    /// Record that a record could not be read as the format the request named.
    fn parsing_error(&mut self, message: String, record_num: u64) {
        self.reading.parsing_errors = self.reading.parsing_errors.saturating_add(1);
        self.log(PARSING_ERROR_CODE, message, record_num);
        self.stop_if_unforgiving();
    }

    /// Record that a value did not read as the type its column declared.
    fn datatype_error(&mut self, message: String, record_num: u64) {
        self.reading.datatype_mismatch_errors =
            self.reading.datatype_mismatch_errors.saturating_add(1);
        self.log(DATATYPE_MISMATCH_ERROR_CODE, message, record_num);
        self.stop_if_unforgiving();
    }

    /// Append one entry to the job's error log, keeping the log bounded.
    fn log(&mut self, code: &str, message: String, record_num: u64) {
        if self.reading.error_logs.len() >= MAX_LOAD_ERROR_LOGS {
            return;
        }
        let file_name = self.file_name.clone();
        self.reading
            .error_logs
            .push(LoadErrorLog::new(code, message, file_name, record_num));
    }

    /// Stop the load where it is when `failOnError` leaves it no choice: "When this
    /// parameter is set to TRUE, the loader stops as soon as it encounters an error. Data
    /// loaded up to that point persists."
    fn stop_if_unforgiving(&mut self) {
        if self.fail_on_error {
            self.stopped = true;
            self.reading.incomplete = true;
        }
    }

    /// Charge `bytes` of element data against this load's budget, or stop it.
    fn charge(&mut self, bytes: usize) -> bool {
        self.decoded = self.decoded.saturating_add(bytes);
        if self.decoded > MAX_LOAD_DECODED_BYTES {
            let ceiling = MAX_LOAD_DECODED_BYTES;
            self.log(
                PARSING_ERROR_CODE,
                format!(
                    "The source's data is larger than the {ceiling} bytes of element data \
                     this emulator loads in one load."
                ),
                self.reading.records,
            );
            self.stopped = true;
            self.reading.incomplete = true;
            return false;
        }
        true
    }
}

/// How this loader reads a delimited object of the load `request` asks for.
fn read_options(request: &LoadRequest) -> ::service_common::graph_bulk_load::ReadOptions {
    use ::service_common::graph_bulk_load::{LoadColumns, ReadOptions};

    ReadOptions {
        columns: if request.format == FORMAT_OPEN_CYPHER {
            LoadColumns::OPEN_CYPHER
        } else {
            LoadColumns::GREMLIN
        },
        // A relationship file of the openCypher format may leave the id column out when the
        // load says its edge ids are generated, so the header alone does not decide whether
        // the file names elements.
        header_requires_id: false,
        // "When userProvidedEdgeIds is absent or set to True, an :ID column must be present
        // in every relationship file in the load."
        edge_ids_from_data: request.user_provided_edge_ids,
        allow_empty_strings: request.allow_empty_strings,
        // "the loader tries to load all the data in the location specified, skipping any
        // entries with errors": the entry a value's own error belongs to is the record, so
        // a record this reading cannot take whole is not loaded.
        skip_record_on_fault: true,
        max_columns: MAX_LOAD_COLUMNS,
        value_chars: MAX_LOAD_VALUE_CHARS,
    }
}

/// Read one object of the source in the format the load named.
pub fn read_object(builder: &mut LoadBuilder, request: &LoadRequest, object: &[u8]) {
    use ::service_common::graph_bulk_load::{read_delimited, read_rdf, RdfSyntax};

    match request.format.as_str() {
        FORMAT_NTRIPLES => read_rdf(
            builder,
            RdfSyntax::NTriples,
            &request.base_uri,
            object,
            MAX_LOAD_VALUE_CHARS,
        ),
        FORMAT_NQUADS => read_rdf(
            builder,
            RdfSyntax::NQuads,
            &request.base_uri,
            object,
            MAX_LOAD_VALUE_CHARS,
        ),
        FORMAT_TURTLE => read_rdf(
            builder,
            RdfSyntax::Turtle,
            &request.base_uri,
            object,
            MAX_LOAD_VALUE_CHARS,
        ),
        // The two delimited formats, which differ only in the columns they spell an element
        // with. The member targets the modelled `Format` enum, so the boundary's constraint
        // walk has already refused every value outside it, and the one format this loader
        // does not read is answered before a load is admitted ([`reads_format`]).
        _ => read_delimited(builder, &read_options(request), object),
    }
}

/// Every element and every fault the shared format reader yields is taken here, where this
/// service's own counters, ceilings and sentences are.
impl ::service_common::graph_bulk_load::LoadSink for LoadBuilder {
    fn document(&self) -> usize {
        self.document
    }

    fn admit_record(&mut self) -> bool {
        if self.stopped {
            return false;
        }
        if self.reading.records >= MAX_LOAD_RECORDS {
            let records = MAX_LOAD_RECORDS;
            self.log(
                PARSING_ERROR_CODE,
                format!(
                    "The source holds more records than this emulator reads in one load: \
                     {records}."
                ),
                self.reading.records,
            );
            self.stopped = true;
            self.reading.incomplete = true;
            return false;
        }
        self.reading.records = self.reading.records.saturating_add(1);
        true
    }

    fn take_node(
        &mut self,
        id: String,
        labels: Vec<String>,
        properties: Vec<(String, Value)>,
        record_num: u64,
    ) {
        let charged = id.len()
            + labels.iter().map(String::len).sum::<usize>()
            + properties
                .iter()
                .map(|(key, value)| key.len() + value_bytes(value))
                .sum::<usize>();
        if !self.charge(charged) {
            return;
        }
        let file_name = self.file_name.clone();
        self.reading.elements.push(LoadedElement::Node {
            id,
            labels,
            properties: properties.into_iter().collect(),
            file_name,
            record_num,
        });
    }

    fn take_edge(
        &mut self,
        id: Option<String>,
        label: String,
        from: String,
        to: String,
        properties: Vec<(String, Value)>,
        record_num: u64,
    ) {
        let charged = id.as_ref().map_or(0, String::len)
            + label.len()
            + from.len()
            + to.len()
            + properties
                .iter()
                .map(|(key, value)| key.len() + value_bytes(value))
                .sum::<usize>();
        if !self.charge(charged) {
            return;
        }
        let file_name = self.file_name.clone();
        self.reading.elements.push(LoadedElement::Edge {
            id,
            rel_type: label,
            from,
            to,
            properties: properties.into_iter().collect(),
            file_name,
            record_num,
        });
    }

    /// What the loader's error log SAYS about a fault the reader reported. A value that
    /// does not read as its column's declared type is the one the loader counts as a
    /// datatype mismatch; every other fault is a record it could not read.
    fn note_fault(
        &mut self,
        fault: &::service_common::graph_bulk_load::LoadFault,
        record_num: u64,
    ) {
        use ::service_common::graph_bulk_load::LoadFault;

        match fault {
            LoadFault::UndeclaredType { key, declared } => self.datatype_error(
                format!("Column {key:?}: the value does not read as the declared {declared}."),
                record_num,
            ),
            LoadFault::FileUnreadable => self.parsing_error(
                "The file could not be read in the format the load named.".to_string(),
                record_num,
            ),
            LoadFault::TooManyColumns { ceiling } => self.parsing_error(
                format!("The file declares more columns than this emulator reads in one: {ceiling}."),
                record_num,
            ),
            LoadFault::HeaderWithoutId { column } => self.parsing_error(
                format!(
                    "The file's header declares no {column} column and no edge endpoint \
                     columns, so none of its records names an element."
                ),
                record_num,
            ),
            LoadFault::RecordUnreadable => self.parsing_error(
                "The record could not be read in the format the load named.".to_string(),
                record_num,
            ),
            LoadFault::NoElementId => {
                self.parsing_error("The record names no element id.".to_string(), record_num)
            }
            LoadFault::EdgeWithoutLabel => self.parsing_error(
                "The record is an edge carrying no label.".to_string(),
                record_num,
            ),
            LoadFault::EdgeWithoutId => self.parsing_error(
                "The record is an edge naming no id, and the load was not told that its edge \
                 ids are generated."
                    .to_string(),
                record_num,
            ),
            LoadFault::OneEndpointOnly => self.parsing_error(
                "The record names one endpoint of an edge, not both.".to_string(),
                record_num,
            ),
        }
    }
}

/// How many bytes one stored property value is charged: its text, the text of every value
/// an array-valued one holds, or the few a number or a boolean occupies.
fn value_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        // An array-valued column is charged for what it HOLDS, so a cell carrying many
        // values is charged for all of them.
        Value::Array(items) => items.iter().map(value_bytes).sum(),
        _ => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The load request these readings are made for: the Gremlin CSV format, halting on
    /// nothing, so what stops a reading is a ceiling of this loader's own.
    fn forgiving_request() -> LoadRequest {
        LoadRequest {
            source: "s3://bucket/data/".to_string(),
            format: FORMAT_CSV.to_string(),
            region: "us-east-1".to_string(),
            mode: DEFAULT_MODE.to_string(),
            fail_on_error: false,
            update_single_cardinality_properties: false,
            edge_only_load: false,
            user_provided_edge_ids: true,
            allow_empty_strings: false,
            base_uri: DEFAULT_BASE_URI.to_string(),
            named_graph_uri: DEFAULT_NAMED_GRAPH_URI.to_string(),
        }
    }

    /// RULE: a source holding more records than this emulator reads in one load is read up
    /// to that ceiling and no further, and the load that stopped there did not finish — so
    /// it cannot report that it completed.
    #[test]
    fn a_source_past_the_record_ceiling_stops_the_load_short() {
        let request = forgiving_request();
        let mut rows = String::from("~id,~label\n");
        for record in 0..=MAX_LOAD_RECORDS {
            rows.push_str(&format!("v{record},Person\n"));
        }
        let mut builder = LoadBuilder::new(&request);
        builder.begin_object("s3://bucket/data/vertices.csv");
        read_object(&mut builder, &request, rows.as_bytes());
        assert!(
            builder.stopped(),
            "a source past the record ceiling must stop the load"
        );
        let reading = builder.finish();
        assert_eq!(
            reading.records, MAX_LOAD_RECORDS,
            "expected the ceiling's worth of records, got {}",
            reading.records
        );
        assert_eq!(
            reading.elements.len(),
            usize::try_from(MAX_LOAD_RECORDS).unwrap_or(usize::MAX),
            "expected one element per record read, got {}",
            reading.elements.len()
        );
        assert!(
            reading.incomplete,
            "a load that stopped short did not finish, so the reading is incomplete"
        );
        assert_eq!(
            reading
                .error_logs
                .last()
                .map(|log| log.error_code.clone())
                .unwrap_or_default(),
            PARSING_ERROR_CODE,
            "the ceiling it met must be in the job's error log, got {:?}",
            reading.error_logs
        );
    }

    /// RULE: a record the reader could not read is counted and worded by this loader, and a
    /// load that is told to skip such entries goes on reading the records after it.
    #[test]
    fn a_record_that_cannot_be_read_is_counted_and_logged() {
        let request = forgiving_request();
        let mut builder = LoadBuilder::new(&request);
        builder.begin_object("s3://bucket/data/vertices.csv");
        read_object(
            &mut builder,
            &request,
            b"~id,~label,age:Int\nv1,Person,30\n,Person,31\nv3,Person,not-a-number\nv4,Person,41\n",
        );
        let reading = builder.finish();
        assert_eq!(
            (reading.records, reading.elements.len()),
            (4, 2),
            "expected every record read and the two well-formed ones loaded, got {:?}",
            reading.elements
        );
        assert_eq!(
            (reading.parsing_errors, reading.datatype_mismatch_errors),
            (1, 1),
            "expected one record with no id and one value that does not read as its \
             declared type, got {reading:?}"
        );
        assert!(
            !reading.incomplete,
            "a load that skipped the entries with errors still read everything its source \
             held"
        );
        let codes: Vec<String> = reading
            .error_logs
            .iter()
            .map(|log| log.error_code.clone())
            .collect();
        assert_eq!(
            codes.as_slice(),
            [
                PARSING_ERROR_CODE.to_string(),
                DATATYPE_MISMATCH_ERROR_CODE.to_string()
            ]
            .as_slice(),
            "expected the loader's own error codes, got {codes:?}"
        );
    }
}
