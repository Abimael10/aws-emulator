// SPDX-License-Identifier: CC0-1.0

//! The LABELS a machine learning transform has been taught — "examples of truth… used to
//! teach the machine learning transform and improve its quality"
//! (`StartImportLabelsTaskRun`) — and the label FILE they travel in.
//!
//! One place, shared by the two task runs that move labels between a transform and Amazon
//! S3: the import reads a label file out of the object store and folds it into the set the
//! transform holds, and the export writes that same set back out as a label file. So a
//! label the import stored is a label the export writes, cell for cell, and neither one
//! reads or renders the file its own way.
//!
//! The file is comma-separated text whose FIRST row names its columns, which is the
//! labelling file the active learning workflow passes around: Glue generates it
//! (`StartMLLabelingSetGenerationTaskRun`), a human fills its label column in, and the
//! caller uploads it again (`StartImportLabelsTaskRun`). It is read and written with the
//! `csv` crate, the one delimited reading this service makes of a caller's data
//! ([`super::column_statistics_source::delimited_rows`] does the same for a table's), so
//! a quoted cell, an embedded separator and an embedded newline survive the round trip.

use super::*;

/// The first column of a labelling file: the labelling SET a labelled row belongs to, which
/// is how the workflow groups the rows a human was asked about together.
///
/// Only the header of an EMPTY set is built from this ([`labelling_file_columns`]): a file
/// that was uploaded carries its own header row, and that is the header the set is held
/// under, so a caller labelling under other column names is not corrected.
pub const LABEL_FILE_LABELING_SET_ID_COLUMN: &str = "labeling_set_id";

/// The second column of a labelling file: the label itself, the answer to "what is the
/// correct way to group these rows together into groups composed entirely of matching
/// records?" (`StartImportLabelsTaskRun`). See
/// [`LABEL_FILE_LABELING_SET_ID_COLUMN`] for when this spelling is used.
pub const LABEL_FILE_LABEL_COLUMN: &str = "label";

/// The separator a label file's cells are written and read with: the comma of the
/// comma-separated label file the labelling workflow passes around.
const LABEL_FILE_DELIMITER: u8 = b',';

/// How many labelled rows ONE transform holds.
///
/// CHOSEN, not an AWS quota: the labels are uploaded by callers, so the set a transform
/// carries is bounded here, exactly as the run history on the same record is
/// ([`MAX_TASK_RUNS_PER_TRANSFORM`]).
///
/// It is enforced in the two places labels arrive, and differently in each. ONE FILE past it
/// is no label file this service imports at all ([`parse_label_file`]), so the run that read
/// it comes to rest having imported nothing rather than part of it — a caller whose upload
/// is too large is told so rather than given a silent prefix of it. COMBINING uploads, each
/// admissible on its own, is bounded on the SET ([`MlLabelSet::absorb`]): the transform keeps
/// the most recently uploaded labels up to this many and the oldest go, the way the run
/// history on the same record drops its oldest run.
pub const MAX_LABELS_PER_TRANSFORM: usize = 50_000;

/// How many COLUMNS a label file names.
///
/// A labelling file names the labelling set id, the label, and then the columns of the data
/// being labelled ([`labelling_file_columns`]) — and the data columns are the transform's
/// own `Schema`, which "has an upper bound of 100 columns"
/// ([`MAX_TRANSFORM_SCHEMA_COLUMNS`], the modelled length of `MLTransform$Schema`). So this
/// is that bound plus the file's own two columns: a header naming more columns than the
/// widest transform can run against is no label file this service imports, and is refused
/// BEFORE its column list is built — a caller writes the width of a header, and a wide
/// header multiplies every row under it.
pub const MAX_LABEL_FILE_COLUMNS: usize = MAX_TRANSFORM_SCHEMA_COLUMNS + 2;

/// How many CELLS one transform's labels amount to: the product of the labels it holds and
/// the columns each is labelled under, which is the memory a label set really takes —
/// neither factor bounds it alone, because a caller writes both the rows of a label file and
/// the width of its header.
///
/// CHOSEN, for the reason [`MAX_LABELS_PER_TRANSFORM`] gives, and the same way the one other
/// reading this service makes of caller-written data bounds its own work
/// ([`MAX_COLUMN_STATISTICS_CELLS`]). Every walk over a label set is bounded by it, and it
/// is evaluated BEFORE the walk allocates rather than after — which is also what bounds the
/// work ONE import does while the store's guard is held ([`MlLabelSet::absorb`] moves cells
/// rather than copying them, so that work is this many pointer moves at the very most).
pub const MAX_LABEL_CELLS: usize = 500_000;

/// How many labels a set under a header of `columns` columns holds: the smaller of the two
/// bounds above, so neither a very long file nor a very wide one is imported past them.
#[must_use]
pub fn max_labels_under(columns: usize) -> usize {
    MAX_LABELS_PER_TRANSFORM.min(MAX_LABEL_CELLS / columns.max(1))
}

/// The media type a label file is stored with: the file is comma-separated text, which is
/// what a client fetching the exported object has to read it as.
const LABEL_FILE_CONTENT_TYPE: &str = "text/csv";

/// The labels one machine learning transform holds, as the label file they were read from
/// laid them out: the column names of its header row, then one labelled row per label.
///
/// Held as the TEXT of each cell rather than as a parsed label, because what the service
/// does with a label file is store it and hand it back: "users upload their
/// answers/labels with a call to StartImportLabelsTaskRun", and
/// `StartExportLabelsTaskRun` "export[s] all labeled data for a particular transform". A
/// cell is never interpreted here, so no label is lost to a reading of it.
#[derive(Debug, Clone, Default)]
pub struct MlLabelSet {
    /// The column names of the set's header row, in file order.
    pub columns: Vec<String>,
    /// One labelled row per label, each holding one cell per column of
    /// [`Self::columns`], in the order they were imported.
    pub rows: Vec<Vec<String>>,
}

impl MlLabelSet {
    /// How many labels this set holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether this set holds no label at all, which is what a transform nothing has
    /// uploaded labels for carries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Fold an uploaded label file into this set, as `StartImportLabelsTaskRun` describes
    /// the two ways it can be uploaded.
    ///
    /// `replace` is that request's `ReplaceAllLabels`: "By default,
    /// StartMLLabelingSetGenerationTaskRun continually learns from and combines all labels
    /// that you upload unless you set Replace to true. If you set Replace to true,
    /// StartImportLabelsTaskRun deletes and forgets all previously uploaded labels and
    /// learns only from the exact set that you upload."
    ///
    /// The uploaded file's own header is the header the set is held under from here on, and
    /// the labels already held are carried over cell by cell, matched by COLUMN NAME — so
    /// combining two files whose columns are ordered differently, or that name a column the
    /// other does not, loses no label and mixes no two columns up.
    ///
    /// The set is bounded by [`max_labels_under`] the columns it ends up held under, and the
    /// labels already held are cut to that bound BEFORE they are carried over, so the walk
    /// that carries them never allocates more than [`MAX_LABEL_CELLS`] cells. The cells are
    /// MOVED rather than copied, and which column each goes to is resolved once for the
    /// whole walk rather than once per row.
    ///
    /// Combining past that bound keeps the labels uploaded MOST RECENTLY and drops the
    /// oldest, which is what [`MAX_LABELS_PER_TRANSFORM`] states for a set and the reason the
    /// run history on the same record gives for dropping its oldest run. The file an import
    /// read is never partly imported: a single file past the bound is refused whole, before
    /// this is reached ([`parse_label_file`]).
    pub fn absorb(&mut self, uploaded: MlLabelSet, replace: bool) {
        if replace || self.columns.is_empty() {
            *self = uploaded;
        } else if uploaded.columns == self.columns {
            self.rows.extend(uploaded.rows);
        } else {
            // The bound on the set under the UPLOADED file's columns decides how many of
            // the labels held can be carried over at all, so it is applied first: the
            // oldest go, for the reason the clamp below gives.
            self.cut_to(max_labels_under(uploaded.columns.len()));
            // Where each column of the new header is to be found in the old one, resolved
            // once for the walk.
            let sources: Vec<Option<usize>> = uploaded
                .columns
                .iter()
                .map(|column| self.columns.iter().position(|held| held == column))
                .collect();
            let mut held = ::std::mem::take(&mut self.rows);
            let carried: Vec<Vec<String>> = held
                .iter_mut()
                .map(|row| project_row(&sources, row))
                .collect();
            self.columns = uploaded.columns;
            self.rows = carried;
            self.rows.extend(uploaded.rows);
        }
        self.cut_to(max_labels_under(self.columns.len()));
    }

    /// Keep at most `capacity` labels, dropping the OLDEST first — for the reason the run
    /// history on the same record drops its oldest run: the labels uploaded most recently
    /// are the ones the caller is teaching the transform with now.
    fn cut_to(&mut self, capacity: usize) {
        if self.rows.len() > capacity {
            let overflow = self.rows.len() - capacity;
            self.rows.drain(..overflow);
        }
    }

    /// This set as the label file an export writes: the header row, then every labelled
    /// row, comma-separated.
    ///
    /// A set holding no label at all is written as the header of a labelling file over
    /// `schema`, the columns the transform runs against
    /// ([`labelling_file_columns`]) — the file "all labeled data for a particular
    /// transform" amounts to when nothing has been labelled yet, rather than an empty
    /// object that names no column.
    #[must_use]
    pub fn render(&self, schema: &[SchemaColumn]) -> Vec<u8> {
        let mut writer = ::csv::WriterBuilder::new()
            .delimiter(LABEL_FILE_DELIMITER)
            .from_writer(Vec::new());
        let columns = if self.columns.is_empty() {
            labelling_file_columns(schema)
        } else {
            self.columns.clone()
        };
        // A `csv` write fails only on its writer, and this one writes into memory.
        let _ = writer.write_record(&columns);
        for row in &self.rows {
            let _ = writer.write_record(row);
        }
        writer.into_inner().unwrap_or_default()
    }
}

/// `row` laid out under a new header instead: `sources` holds, per column of that header,
/// where in `row` its cell is to be found, and a column the old header did not name holds no
/// cell. The cells are MOVED out of `row`, which is left holding none of them.
fn project_row(sources: &[Option<usize>], row: &mut [String]) -> Vec<String> {
    sources
        .iter()
        .map(|source| {
            source
                .and_then(|index| row.get_mut(index).map(::std::mem::take))
                .unwrap_or_default()
        })
        .collect()
}

/// The header of a labelling file over `schema`: the labelling set id, the label, and then
/// the columns of the data being labelled.
///
/// The data columns come from the transform's own `Schema`, "the columns and data types
/// that this transform can run against" (`MLTransform$Schema`), so the file names the
/// columns a labeller is shown rather than anything of a request.
#[must_use]
pub fn labelling_file_columns(schema: &[SchemaColumn]) -> Vec<String> {
    let mut columns = vec![
        LABEL_FILE_LABELING_SET_ID_COLUMN.to_string(),
        LABEL_FILE_LABEL_COLUMN.to_string(),
    ];
    columns.extend(schema.iter().filter_map(|column| column.name.clone()));
    columns
}

/// The labels a label file's bytes hold, or why they are no label file.
///
/// The FIRST row names the columns and every row after it is one label. A row carrying
/// fewer cells than the header names columns leaves the rest holding no value — which is how
/// a delimited reader resolves a row that does not match its header, and is the reading this
/// service already gives a table's text rows
/// ([`super::column_statistics_source::delimited_rows`] reads them flexibly too). A row
/// carrying MORE is no label the file's own header accounts for, so the file imports
/// nothing: a cell under no column is not a cell of a label, and silently dropping it would
/// lose what the caller uploaded. Every stored row is therefore exactly as wide as the
/// header, which is what lets the export render them back under it and a later upload carry
/// them over column by column.
///
/// Both of the caller-written figures that decide how much this allocates are bounded
/// BEFORE anything is built from them: the width of the header by
/// [`MAX_LABEL_FILE_COLUMNS`] (and every row by the header), and how many labels follow by
/// [`max_labels_under`] that width. A file past either is no label file this service
/// imports, and the run that read it imports nothing rather than part of it.
pub fn parse_label_file(bytes: &[u8]) -> Result<MlLabelSet, String> {
    let mut reader = ::csv::ReaderBuilder::new()
        .delimiter(LABEL_FILE_DELIMITER)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut records = reader.records();
    let Some(header) = records.next() else {
        return Err("the label file is empty, so it names no column to label under".to_string());
    };
    let header = header.map_err(|error| format!("the label file's header could not be read: {error}"))?;
    if header.is_empty() {
        return Err("the label file is empty, so it names no column to label under".to_string());
    }
    // The width is checked on the RECORD, before a column list is built from it.
    if header.len() > MAX_LABEL_FILE_COLUMNS {
        return Err(too_wide("the label file's header", header.len()));
    }
    let columns: Vec<String> = header.iter().map(str::to_string).collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let capacity = max_labels_under(columns.len());
    for record in records {
        let record =
            record.map_err(|error| format!("a row of the label file could not be read: {error}"))?;
        // A trailing blank line is the end of the file, not a label.
        if record.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        if record.len() > columns.len() {
            return Err(format!(
                "a row of the label file carries {} cells, more than the {} columns its header \
                 names",
                record.len(),
                columns.len()
            ));
        }
        let mut row: Vec<String> = record.iter().map(str::to_string).collect();
        row.resize(columns.len(), String::new());
        rows.push(row);
        if rows.len() > capacity {
            return Err(format!(
                "the label file holds more than the {capacity} labels one transform holds \
                 under its {} columns",
                columns.len()
            ));
        }
    }
    Ok(MlLabelSet { columns, rows })
}

/// Why a header naming more columns than [`MAX_LABEL_FILE_COLUMNS`] imports nothing.
fn too_wide(what: &str, cells: usize) -> String {
    format!(
        "{what} names {cells} columns, more than the {MAX_LABEL_FILE_COLUMNS} one transform is \
         labelled under"
    )
}

/// The bucket and key the label file an IMPORT reads lives at, or `None` for a value that
/// names no such object: one that is no `s3://` location at all, and one that names a bucket
/// without naming an object inside it.
///
/// ONE predicate, so the boundary the START refuses a path at and the boundary the READ
/// fails a run at cannot drift apart: an import reads ONE object, where an export writes
/// under a PREFIX ([`exported_label_file_key`]), which is why only this direction asks for a
/// key.
#[must_use]
pub fn label_file_object(path: &str) -> Option<(String, String)> {
    let (bucket, key) = split_s3_location(path)?;
    (!key.is_empty()).then_some((bucket, key))
}

/// Read the label file at `input_s3_path` out of the emulator's own Amazon S3, or answer
/// why its labels could not be imported.
///
/// The read goes over the in-process callback into this same emulator, through the ONE
/// Amazon S3 client this service reads objects with
/// ([`super::column_statistics_source::s3_client`]) and the one object read beside it, so
/// the object a caller's own `PutObject` stored is the object the import reads, bounded by
/// that read's own ceiling.
///
/// `Err` is what the run that asked for the import comes to rest FAILED with: the reason is
/// about the object rather than about the request, which is why it is not a refusal of the
/// start.
pub async fn read_label_file(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    input_s3_path: &str,
) -> Result<MlLabelSet, String> {
    let Some((bucket, key)) = label_file_object(input_s3_path) else {
        return Err(format!(
            "the label file path {input_s3_path} names no object in the object store"
        ));
    };
    let client = super::column_statistics_source::s3_client(ctx, region);
    let bytes = super::column_statistics_source::read_object(&client, &bucket, &key).await?;
    parse_label_file(&bytes)
}

/// Write `labels` out as the label file of an export under `output_s3_path`, and answer the
/// `s3://` path of the object it was written at.
///
/// The path the request names is where the labels are exported TO, and the object written
/// under it is named for the task run that wrote it — so two exports of one transform leave
/// two files rather than one overwriting the other, and a client that lists the path finds
/// the file by the `TaskRunId` the start handed it.
///
/// `Err` is why the labels could not be exported, which the run comes to rest FAILED with,
/// for the reason [`read_label_file`] gives.
pub async fn write_label_file(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    output_s3_path: &str,
    run_id: &str,
    labels: &[u8],
) -> Result<String, String> {
    let Some((bucket, prefix)) = split_s3_location(output_s3_path) else {
        return Err(format!(
            "the label file path {output_s3_path} names no Amazon S3 bucket"
        ));
    };
    let key = exported_label_file_key(&prefix, run_id);
    super::column_statistics_source::s3_client(ctx, region)
        .put_object()
        .bucket(&bucket)
        .key(&key)
        .content_type(LABEL_FILE_CONTENT_TYPE)
        .body(::aws_sdk_s3::primitives::ByteStream::from(labels.to_vec()))
        .send()
        .await
        .map_err(|error| {
            format!(
                "the object store did not store the label file at s3://{bucket}/{key}: {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
    Ok(format!("s3://{bucket}/{key}"))
}

/// The key an export writes its label file at: the path the request named, as a prefix,
/// then the run's own id and the suffix a comma-separated file carries.
///
/// A prefix that already ends in the separator gains no second one, and a request that
/// named the bucket alone writes at the top of it.
#[must_use]
pub fn exported_label_file_key(prefix: &str, run_id: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("{run_id}.csv")
    } else {
        format!("{trimmed}/{run_id}.csv")
    }
}

impl MlTransformEntry {
    /// Fold an uploaded label file into the labels this transform holds, as
    /// `StartImportLabelsTaskRun` asked for, and answer how many labels it holds then.
    ///
    /// `replace` is that request's `ReplaceAllLabels` ([`MlLabelSet::absorb`] states what
    /// each setting of it does).
    ///
    /// The label COUNT moves with the set: `MLTransform$LabelCount` is read here as the
    /// LABELS the transform has been given — the member's own name, and the quantity the
    /// active learning workflow moves ("as you create a better transform, you can
    /// iteratively download, label, and upload the labeling file"), which is what a client
    /// polling a transform after an import is asking about. The reading is consistent with
    /// the other operation that touches it: a labelling set generation run leaves it where
    /// it is, because that run uploads no label (and, in this emulator, generates no
    /// labelling file either) — so the count only ever moves where labels really arrived,
    /// whichever of the two quantities the member's one ambiguous sentence is read as.
    ///
    /// The transform's DEFINITION is untouched: an import teaches the transform rather than
    /// modifying what it is, so its `Status`, its parameters and its `LastModifiedOn` are
    /// not this call's to move.
    pub fn import_labels(&mut self, uploaded: MlLabelSet, replace: bool) -> usize {
        self.labels.absorb(uploaded, replace);
        self.label_count = i32::try_from(self.labels.len()).unwrap_or(i32::MAX);
        self.labels.len()
    }

    /// The label file "all labeled data for a particular transform"
    /// (`StartExportLabelsTaskRun`) amounts to: the labels this transform holds, rendered
    /// as the file an export writes ([`MlLabelSet::render`] states what a transform holding
    /// none is written as).
    #[must_use]
    pub fn exported_label_file(&self) -> Vec<u8> {
        self.labels.render(&self.schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A label file's cells survive being read and written again, quoting and all: the
    /// rows differ along the cells they hold — an ordinary value, one holding the
    /// separator, one holding a quote, one holding a newline and one holding nothing.
    #[test]
    fn a_label_file_round_trips_through_the_stored_set() {
        let cases: &[(&str, &[&str])] = &[
            ("plain cells", &["set-1", "0", "alice"]),
            ("a cell holding the separator", &["set-1", "0", "a,b"]),
            ("a cell holding a quote", &["set-1", "0", "a\"b"]),
            ("a cell holding a newline", &["set-1", "0", "a\nb"]),
            ("a cell holding nothing", &["set-1", "0", ""]),
        ];

        for (case, cells) in cases {
            let mut set = MlLabelSet {
                columns: vec![
                    LABEL_FILE_LABELING_SET_ID_COLUMN.to_string(),
                    LABEL_FILE_LABEL_COLUMN.to_string(),
                    "name".to_string(),
                ],
                rows: Vec::new(),
            };
            set.rows
                .push(cells.iter().map(|cell| (*cell).to_string()).collect());
            let rendered = set.render(&[]);
            let read = parse_label_file(&rendered)
                .unwrap_or_else(|error| panic!("case {case:?}: the file must read back, got {error}"));
            assert_eq!(
                read.columns, set.columns,
                "case {case:?}: expected the header {:?}, got {:?}",
                set.columns, read.columns
            );
            assert_eq!(
                read.rows, set.rows,
                "case {case:?}: expected the rows {:?}, got {:?}",
                set.rows, read.rows
            );
        }
    }

    /// Combining an upload whose columns are ordered differently keeps every label, each
    /// cell under the column it was labelled for.
    #[test]
    fn combining_reorders_the_labels_already_held() {
        let mut set = MlLabelSet {
            columns: vec!["id".to_string(), "label".to_string()],
            rows: vec![vec!["1".to_string(), "a".to_string()]],
        };
        set.absorb(
            MlLabelSet {
                columns: vec!["label".to_string(), "id".to_string()],
                rows: vec![vec!["b".to_string(), "2".to_string()]],
            },
            false,
        );
        assert_eq!(
            set.rows,
            vec![
                vec!["a".to_string(), "1".to_string()],
                vec!["b".to_string(), "2".to_string()],
            ],
            "the labels already held must be carried over column by column, got {:?}",
            set.rows
        );
    }

    /// Combining past the labels one transform holds keeps the ones uploaded MOST RECENTLY
    /// and drops the oldest, whether the upload is held under the same columns or under its
    /// own.
    #[test]
    fn combining_past_the_bound_keeps_the_labels_uploaded_last() {
        // One column, so the bound on the set is the row bound itself
        // (`max_labels_under(1)`).
        let columns = vec!["label".to_string()];
        let capacity = max_labels_under(columns.len());
        let cases: &[(&str, Vec<String>, &str)] = &[
            // Under the same columns each carried label keeps its cell; under columns of the
            // upload's own naming a carried label has no cell under them, which is the
            // projection's own rule — what this row pins is WHICH labels are kept.
            ("the same columns", columns.clone(), "held-1"),
            ("columns of its own", vec!["answer".to_string()], ""),
        ];

        for (case, uploaded_columns, oldest_kept) in cases {
            let mut set = MlLabelSet {
                columns: columns.clone(),
                rows: (0..capacity).map(|row| vec![format!("held-{row}")]).collect(),
            };
            set.absorb(
                MlLabelSet {
                    columns: uploaded_columns.clone(),
                    rows: vec![vec!["newest".to_string()]],
                },
                false,
            );
            assert_eq!(
                set.len(),
                capacity,
                "case {case:?}: expected the set to hold its {capacity} labels, got {}",
                set.len()
            );
            assert_eq!(
                set.rows.last().map(Vec::as_slice),
                Some(["newest".to_string()].as_slice()),
                "case {case:?}: expected the label uploaded last to be held, got {:?}",
                set.rows.last()
            );
            assert_eq!(
                set.rows.first().map(Vec::as_slice),
                Some([(*oldest_kept).to_string()].as_slice()),
                "case {case:?}: expected the oldest label held to be {oldest_kept:?}, got {:?}",
                set.rows.first()
            );
        }
    }

    /// Where an export writes its file, for the forms a path can take.
    #[test]
    fn an_exported_file_is_named_for_its_run_under_the_path() {
        let cases: &[(&str, &str, &str)] = &[
            ("a prefix", "labels/out", "labels/out/run-1.csv"),
            ("a prefix ending in the separator", "labels/out/", "labels/out/run-1.csv"),
            ("the bucket alone", "", "run-1.csv"),
        ];
        for (case, prefix, expected) in cases {
            let key = exported_label_file_key(prefix, "run-1");
            assert_eq!(
                key, *expected,
                "case {case:?}: expected the key {expected:?}, got {key:?}"
            );
        }
    }

    /// A label file is read at the widest header it may name and refused one column past
    /// it, so the width a caller writes cannot multiply the labels held without bound.
    #[test]
    fn a_header_past_the_widest_a_label_file_names_imports_nothing() {
        let cases: &[(&str, usize, bool)] = &[
            ("a labelling file's own columns", 3, true),
            ("the widest header", MAX_LABEL_FILE_COLUMNS, true),
            ("one column past it", MAX_LABEL_FILE_COLUMNS + 1, false),
        ];
        for (case, columns, admitted) in cases {
            let header: Vec<String> = (0..*columns).map(|index| format!("c{index}")).collect();
            let file = format!("{}\n", header.join(","));
            let read = parse_label_file(file.as_bytes());
            assert_eq!(
                read.is_ok(),
                *admitted,
                "case {case:?}: expected the file to be admitted: {admitted}, got {read:?}"
            );
            if let Ok(set) = read {
                assert_eq!(
                    set.columns.len(),
                    *columns,
                    "case {case:?}: expected {columns} columns, got {:?}",
                    set.columns.len()
                );
            }
        }
    }

    /// A row is read under the columns its file's header names: a short one leaves the rest
    /// holding no value, and one past the header imports nothing.
    #[test]
    fn a_row_is_read_under_the_columns_its_header_names() {
        let cases: &[(&str, &str, Option<&[&str]>)] = &[
            (
                "a row of the header's own width",
                "a,b,c\n1,2,3\n",
                Some(&["1", "2", "3"]),
            ),
            (
                "a row short of the header",
                "a,b,c\n1,2\n",
                Some(&["1", "2", ""]),
            ),
            ("a row past the header", "a,b,c\n1,2,3,4\n", None),
        ];
        for (case, file, expected) in cases {
            let read = parse_label_file(file.as_bytes());
            match expected {
                Some(cells) => {
                    let set = read.unwrap_or_else(|error| {
                        panic!("case {case:?}: the file must read back, got {error}")
                    });
                    let row: Vec<String> = cells.iter().map(|cell| (*cell).to_string()).collect();
                    assert_eq!(
                        set.rows,
                        vec![row.clone()],
                        "case {case:?}: expected the row {row:?}, got {:?}",
                        set.rows
                    );
                }
                None => assert!(
                    read.is_err(),
                    "case {case:?}: expected the file to import nothing, got {read:?}"
                ),
            }
        }
    }

    /// How many labels a set holds, for headers of several widths: the row bound where the
    /// header is narrow, and the cell bound where it is wide.
    #[test]
    fn a_wider_header_holds_fewer_labels() {
        let cases: &[(&str, usize, usize)] = &[
            // A header naming no column at all is bounded as a single column would be.
            ("no column at all", 0, MAX_LABELS_PER_TRANSFORM),
            ("one column", 1, MAX_LABELS_PER_TRANSFORM),
            ("a labelling file's three columns", 3, MAX_LABELS_PER_TRANSFORM),
            // Past this width the cells bound is the smaller of the two.
            ("twenty columns", 20, MAX_LABEL_CELLS / 20),
            (
                "the widest header a label file names",
                MAX_LABEL_FILE_COLUMNS,
                MAX_LABEL_CELLS / MAX_LABEL_FILE_COLUMNS,
            ),
        ];
        for (case, columns, expected) in cases {
            let held = max_labels_under(*columns);
            assert_eq!(
                held, *expected,
                "case {case:?}: expected {expected} labels to be held, got {held}"
            );
        }
    }
}
