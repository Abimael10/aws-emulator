// SPDX-License-Identifier: CC0-1.0

//! `awsglue.context`, `awsglue.job`, `awsglue.transforms` and the two sides a frame is read
//! from and written to (`awsglue.data_source`, `awsglue.data_sink`).

/// The source of `awsglue/context.py`.
pub(super) const CONTEXT_SOURCE: &str = r#"
"""The GlueContext a job script reads its data through and writes its output with."""

from awsglue import _table_data
from awsglue.data_sink import DataSink
from awsglue.data_source import DataSource
from awsglue.dynamicframe import DynamicFrame, DynamicFrameReader, DynamicFrameWriter
from pyspark.sql import SparkSession

# The connection options a read of Amazon S3 paths HONOURS: the paths it reads, and whether
# it descends the sub-prefixes under them. Every other option changes what a read DOES, and
# this reader performs none of them, so one it is given is refused naming it.
HONOURED_S3_READ_OPTIONS = ("path", "paths", "recurse")


class _Logger(object):
    """`glueContext.get_logger()`: the logger a script writes its own messages through.

    The messages land in the run's own log, which is what the interpreter collects.
    """

    def _write(self, level, message, *rest):
        text = str(message)
        if rest:
            text = text % rest
        print("%s %s" % (level, text))
        return None

    def info(self, message, *rest):
        return self._write("INFO", message, *rest)

    def warn(self, message, *rest):
        return self._write("WARN", message, *rest)

    def warning(self, message, *rest):
        return self._write("WARN", message, *rest)

    def error(self, message, *rest):
        return self._write("ERROR", message, *rest)

    def debug(self, message, *rest):
        return self._write("DEBUG", message, *rest)


class GlueContext(object):
    """The entry point of a Glue ETL script: where its frames come from and go to."""

    def __init__(self, sparkContext, **options):
        self._sc = sparkContext
        self._session = SparkSession(sparkContext)
        self._logger = _Logger()
        # The options a context is built with tune how its reads are PARTITIONED
        # (`minPartitions`, `targetPartitions`). The work here runs in this one process and
        # a frame is held in one partition, so they change nothing about what a frame holds
        # or what a write leaves behind; they are kept and readable rather than acted on.
        self._options = dict(options or {})

    # --- what the context HOLDS ---------------------------------------------------------

    @property
    def spark_session(self):
        return self._session

    @property
    def sparkSession(self):
        return self._session

    @property
    def _jvm(self):
        return self._session._jvm

    @property
    def _jsc(self):
        return self._session._jsc

    @property
    def sparkContext(self):
        return self._sc

    def get_logger(self):
        return self._logger

    @property
    def create_dynamic_frame(self):
        return DynamicFrameReader(self)

    @property
    def write_dynamic_frame(self):
        return DynamicFrameWriter(self)

    # --- reading ------------------------------------------------------------------------

    def create_dynamic_frame_from_catalog(self, database=None, table_name=None,
                                          redshift_tmp_dir="", transformation_ctx="",
                                          push_down_predicate="", additional_options=None,
                                          catalog_id=None, **kwargs):
        """The frame of one Data Catalog table, read through the catalog and the store."""
        named = database or kwargs.get("name_space")
        table = table_name or kwargs.get("tableName") or kwargs.get("table")
        if not named or not table:
            raise ValueError(
                "a frame read from the catalog names a database and a table, got %r and %r"
                % (named, table)
            )
        # A temporary directory is named when the table being read is a REDSHIFT table,
        # whose data the read unloads there first — a source this library does not read, so
        # a read that names one is refused rather than served from somewhere else.
        if redshift_tmp_dir:
            raise ValueError(
                "this emulator reads a table's data from the objects at its location; the "
                "temporary directory %r is one it does not unload a source into"
                % (redshift_tmp_dir,)
            )
        columns, records = _table_data.read_catalog_table(
            named,
            table,
            catalog_id=catalog_id,
            push_down_predicate=push_down_predicate,
            additional_options=additional_options,
            transformation_ctx=transformation_ctx,
        )
        return DynamicFrame(
            records, self, transformation_ctx or "%s_%s" % (named, table), columns
        )

    def create_dynamic_frame_from_options(self, connection_type=None,
                                          connection_options=None, format=None,
                                          format_options=None, transformation_ctx="",
                                          push_down_predicate="", **kwargs):
        """The frame of the objects one or more Amazon S3 paths hold."""
        options = dict(connection_options or {})
        kind = str(connection_type or "s3").lower()
        if kind not in ("s3", "s3a", "s3n"):
            raise ValueError(
                "this emulator reads a frame from Amazon S3; %r is not a connection type it "
                "reads from" % (connection_type,)
            )
        # A PUSH-DOWN PREDICATE pre-filters a read by the partitions it keeps, which an
        # Amazon S3 path read has none of to discover here: the read takes every object
        # under the paths, so a predicate is refused rather than dropped — a frame holding
        # every row where the script asked for a filtered one is a wrong answer reported as
        # a success.
        if push_down_predicate:
            raise ValueError(
                "this emulator reads every object under the paths it is given; the "
                "push-down predicate %r is a filter it does not apply"
                % (push_down_predicate,)
            )
        _table_data.refuse_unhonoured(
            options,
            HONOURED_S3_READ_OPTIONS,
            "reads a frame from the Amazon S3 paths its connection options name",
        )
        paths = options.get("paths") or ([options["path"]] if options.get("path") else [])
        if not paths:
            raise ValueError("a frame read from Amazon S3 names the paths to read")
        columns, records = _table_data.read_s3_paths(
            paths,
            format,
            format_options,
            # "recurse": whether the read descends the sub-prefixes under the paths it was
            # given, which a read that does not ask for it does not do.
            recurse=_table_data.truthy(options.get("recurse")),
            transformation_ctx=transformation_ctx,
        )
        return DynamicFrame(records, self, transformation_ctx, columns)

    def create_dynamic_frame_from_rdd(self, data, name, schema=None, sample_ratio=None,
                                      transformation_ctx=""):
        """The frame of the rows a dataset of this process holds.

        A sample ratio decides how much of a dataset a reader INFERS a schema from, and the
        frame built here carries the fields its own records hold, so it changes nothing
        about the frame.
        """
        records = []
        for row in data.collect():
            if hasattr(row, "asDict"):
                records.append(row.asDict())
            elif isinstance(row, dict):
                records.append(dict(row))
            else:
                raise ValueError(
                    "a frame built from a dataset reads its records as mappings, got %s"
                    % (type(row).__name__,)
                )
        return DynamicFrame(records, self, name or transformation_ctx)

    def getSource(self, connection_type=None, format=None, transformation_ctx="",
                  push_down_predicate="", **options):
        """The source a frame is read from, carrying what the read was asked for."""
        return DataSource(
            self, connection_type, format, transformation_ctx, options, push_down_predicate
        )

    # --- writing ------------------------------------------------------------------------

    def getSink(self, path=None, connection_type="s3", transformation_ctx="", **options):
        """The sink a frame is written to, as `getSink` hands one out."""
        return DataSink(self, path, connection_type, transformation_ctx, options)

    def write_dynamic_frame_from_options(self, frame, connection_type=None,
                                         connection_options=None, format=None,
                                         format_options=None, transformation_ctx="",
                                         **kwargs):
        """Write a frame to the Amazon S3 path the connection options name."""
        options = dict(connection_options or {})
        kind = str(connection_type or "s3").lower()
        if kind not in ("s3", "s3a", "s3n"):
            raise ValueError(
                "this emulator writes a frame to Amazon S3; %r is not a connection type it "
                "writes to" % (connection_type,)
            )
        path = options.get("path") or (options.get("paths") or [None])[0]
        if not path:
            raise ValueError("a frame written to Amazon S3 names the path to write to")
        sink = DataSink(self, path, kind, transformation_ctx, options)
        sink.setFormat(format, **(format_options or {}))
        return sink.writeFrame(frame)

    def write_dynamic_frame_from_catalog(self, frame, database=None, table_name=None,
                                         transformation_ctx="", additional_options=None,
                                         catalog_id=None, **kwargs):
        """Write a frame to the location the named catalog table declares."""
        if not database or not table_name:
            raise ValueError(
                "a frame written to the catalog names a database and a table, got %r and %r"
                % (database, table_name)
            )
        table = _table_data.table_of(database, table_name, catalog_id)
        location = (table.get("StorageDescriptor") or {}).get("Location")
        if not location:
            raise ValueError(
                "the table %s.%s declares no location to write to" % (database, table_name)
            )
        sink = DataSink(
            self, location, "s3", transformation_ctx, dict(additional_options or {})
        )
        sink.setCatalogInfo(database, table_name, catalog_id)
        sink.setFormat(_table_data.table_format(table))
        return sink.writeFrame(frame)

    # A PURGE of a path or of a table (`purge_s3_path`, `purge_table`) deletes the objects
    # older than the retention period it is given — "files newer than the retention period
    # are retained" — beside the manifest it writes and the storage classes it excludes.
    # Deleting everything under the path instead would be a different and destructive
    # operation, so this library supplies neither: a script that calls one finds the
    # attribute absent, as it does for every other surface this library does not perform.
"#;

/// The source of `awsglue/data_source.py`.
pub(super) const DATA_SOURCE_SOURCE: &str = r#"
"""The source a frame is read from, as `getSource` hands one out."""


class DataSource(object):
    """One source: where it reads from, in which format, and under what filter."""

    def __init__(self, glue_context, connection_type=None, format=None,
                 transformation_ctx="", options=None, push_down_predicate=""):
        self._glue_context = glue_context
        self._connection_type = connection_type
        self._format = format
        self._format_options = {}
        self._transformation_ctx = transformation_ctx
        self._options = dict(options or {})
        self._push_down_predicate = push_down_predicate

    def setFormat(self, format, **options):
        self._format = format
        self._format_options = dict(options or {})
        return self

    def getFrame(self, **options):
        """The frame this source holds."""
        connection_options = dict(self._options)
        connection_options.update(options or {})
        return self._glue_context.create_dynamic_frame_from_options(
            connection_type=self._connection_type,
            connection_options=connection_options,
            format=self._format,
            format_options=self._format_options,
            transformation_ctx=self._transformation_ctx,
            push_down_predicate=self._push_down_predicate,
        )

    def getDynamicFrame(self, **options):
        return self.getFrame(**options)
"#;

/// The source of `awsglue/data_sink.py`.
///
/// What a sink WRITES are real objects of this emulator's own object store, through the
/// shared Spark filesystem a program's own writes go through — so a later read of the path
/// sees exactly what the frame held. A frame written in the columnar format is encoded by
/// the Spark runtime under this library ([`::service_common::spark_runtime`]), so its part
/// files are Apache Parquet files any Parquet reader reads. A sink asked to UPDATE THE
/// CATALOG registers the partitions it wrote with the Data Catalog's own API, so a partition
/// a run reports as written is one the catalog answers for.
pub(super) const DATA_SINK_SOURCE: &str = r#"
"""The sink a frame is written to, as `getSink` hands one out."""

import csv
import io
import json

from awsglue import _table_data
from pyspark import filesystem, parquet

# Whether a write leaves the marker object beside its part files: the setting of the
# environment's own output committer, which for a Glue job's environment leaves none
# ([`pyspark.release`]).
from pyspark.release import MARKS_SUCCESSFUL_JOBS

# The name one write of a frame leaves its records under, beside the marker Spark writes for
# an output that was written in full.
PART_NAME = "part-00000"
SUCCESS_MARKER = "_SUCCESS"

# The formats a frame is written in: the two text layouts, and the columnar one the Spark
# runtime under this library really encodes ([`pyspark.parquet`]). A layout outside these is
# one this emulator cannot write faithfully, and a sink asked for one says so rather than
# leaving behind an object that is not in that format.
TEXT_FORMATS = ("csv", "tsv", "text")
JSON_FORMATS = ("json", "jsonl")
PARQUET_FORMATS = ("parquet", "glueparquet")

# The connection options a sink HONOURS: where it writes, how it partitions what it writes,
# and whether it registers those partitions with the Data Catalog.
HONOURED_OPTIONS = ("path", "paths", "partitionKeys", "enableUpdateCatalog")

# The format options it honours: whether delimited text carries a header line, the delimiter
# between its cells, and the codec a columnar part's pages are stored under.
HONOURED_FORMAT_OPTIONS = ("writeHeader", "separator", "delimiter", "compression")


class UnwritableFormat(Exception):
    """A format this emulator does not write a frame in."""


class DataSink(object):
    """One sink: where a frame goes, in which format, and what it tells the catalog."""

    def __init__(self, glue_context, path=None, connection_type="s3",
                 transformation_ctx="", options=None):
        self._glue_context = glue_context
        self._options = dict(options or {})
        self._path = path or self._options.get("path")
        self._connection_type = str(connection_type or "s3").lower()
        self._transformation_ctx = transformation_ctx
        self._format = None
        self._format_options = {}
        self._catalog_database = None
        self._catalog_table = None
        self._catalog_id = None

    def setFormat(self, format, **options):
        self._format = format
        self._format_options = dict(options or {})
        return self

    def setCatalogInfo(self, catalogDatabase=None, catalogTableName=None, catalogId=None):
        self._catalog_database = catalogDatabase
        self._catalog_table = catalogTableName
        self._catalog_id = catalogId
        return self

    def setAccumulableSize(self, size):
        return self

    def withFormat(self, format, **options):
        return self.setFormat(format, **options)

    def write(self, dynamic_frame, info=""):
        return self.writeFrame(dynamic_frame, info)

    def writeFrame(self, dynamic_frame, info=""):
        """Write the frame's records out, and register what was written if asked to."""
        if self._connection_type not in ("s3", "s3a", "s3n"):
            raise UnwritableFormat(
                "this emulator writes a frame to Amazon S3; %r is not a connection type it "
                "writes to" % (self._connection_type,)
            )
        # An option changes what the write DOES — a compression it would have to apply, an
        # update behaviour it would have to follow — so one this sink performs nothing for is
        # refused naming it rather than dropped, which would leave an object that is not
        # what the script asked for and report a success for it.
        _table_data.refuse_unhonoured(
            self._options, HONOURED_OPTIONS, "writes a frame as the objects of one path"
        )
        _table_data.refuse_unhonoured(
            self._format_options,
            HONOURED_FORMAT_OPTIONS,
            "writes a frame as delimited text, as JSON documents or as Parquet",
        )
        if not self._path:
            raise ValueError("a sink names the Amazon S3 path it writes to")
        kind = str(self._format or "json").lower()
        partition_keys = [
            str(key) for key in (self._options.get("partitionKeys") or []) if str(key)
        ]
        records = dynamic_frame.records
        columns = [name for name, _declared in dynamic_frame._columns]
        declared = dict(dynamic_frame._columns)
        prefix = str(self._path).rstrip("/")
        written = []
        for values, group in _grouped(records, partition_keys):
            held = [name for name in columns if name not in partition_keys]
            # The value of a partition key is DATA — a field of the records being written —
            # so it goes into the path through the layout's own escaping: a value spelling a
            # path separator or a parent directory may not name an object outside the path
            # the sink was given ([`pyspark.filesystem.escape_path_name`]).
            within = "/".join(
                "%s=%s"
                % (
                    filesystem.escape_path_name(key),
                    filesystem.escape_path_name(values[index]),
                )
                for index, key in enumerate(partition_keys)
            )
            at = "%s/%s" % (prefix, within) if within else prefix
            if kind in PARQUET_FORMATS:
                # The part is the Parquet file its records make: the values themselves in
                # its pages, and the row count, the schema and each column's bounds in its
                # footer, encoded by the Spark runtime under this library.
                compression = self._format_options.get("compression", "snappy")
                _codec, suffix = parquet.codec_of(compression)
                filesystem.write_bytes(
                    "%s/%s%s.parquet" % (at, PART_NAME, suffix),
                    parquet.file_bytes(
                        held,
                        [
                            [record.get(name) for name in held]
                            for record in group
                        ],
                        declared,
                        compression,
                    ),
                )
            else:
                body = _rendered(group, held, kind, self._format_options)
                filesystem.write_text(
                    "%s/%s.%s" % (at, PART_NAME, _suffix(kind)), body
                )
            if MARKS_SUCCESSFUL_JOBS:
                filesystem.write_text("%s/%s" % (at, SUCCESS_MARKER), "")
            written.append((values, at))
        if self._options.get("enableUpdateCatalog") and self._catalog_table:
            _register(
                self._catalog_database,
                self._catalog_table,
                self._catalog_id,
                partition_keys,
                written,
            )
        return dynamic_frame


def _suffix(kind):
    """The extension a written part carries, which is the format it is in."""
    if kind in JSON_FORMATS:
        return "json"
    if kind == "tsv":
        return "tsv"
    return "csv"


def _grouped(records, partition_keys):
    """The records by the values of the partition keys, in the order they first appear."""
    if not partition_keys:
        return [((), list(records))]
    order = []
    grouped = {}
    for record in records:
        values = tuple(str(record.get(key, "")) for key in partition_keys)
        if values not in grouped:
            grouped[values] = []
            order.append(values)
        grouped[values].append(record)
    return [(values, grouped[values]) for values in order]


def _rendered(records, columns, kind, format_options):
    """The text one part file holds: the records, in the format the sink was given."""
    if kind in JSON_FORMATS:
        return "".join(
            "%s\n" % (json.dumps(dict((name, record.get(name)) for name in columns)),)
            for record in records
        )
    if kind not in TEXT_FORMATS:
        raise UnwritableFormat(
            "this emulator writes a frame as delimited text, as JSON documents or as "
            "Parquet; %r is a layout it cannot write" % (kind,)
        )
    delimiter = "\t" if kind == "tsv" else str(
        format_options.get("separator") or format_options.get("delimiter") or ","
    )[:1]
    # Written by the delimited-text writer of the standard library, so a value holding the
    # delimiter, a quote or a newline is quoted the way a reader of that text expects.
    held = io.StringIO()
    writer = csv.writer(held, delimiter=delimiter, lineterminator="\n")
    if format_options.get("writeHeader", True):
        writer.writerow(columns)
    for record in records:
        writer.writerow(
            [
                "" if record.get(name) is None else record.get(name)
                for name in columns
            ]
        )
    return held.getvalue()


def _register(database, table_name, catalog_id, partition_keys, written):
    """Register the partitions a write left behind with the Data Catalog."""
    if not partition_keys:
        return None
    # The Data Catalog client the RUNTIME supplies the guest, through this library's one
    # construction of it: its transport is this emulator's own in-process callback, signed
    # as the role the run is performed as — it resolves no endpoint, opens no connection and
    # reads no credential of the host.
    glue = _table_data.glue_client()
    request = {"DatabaseName": database, "Name": table_name}
    if catalog_id:
        request["CatalogId"] = catalog_id
    table = glue.get_table(**request)["Table"]
    descriptor = dict(table.get("StorageDescriptor") or {})
    for values, at in written:
        partition = {
            "Values": list(values),
            "StorageDescriptor": dict(descriptor, Location=at),
        }
        call = {
            "DatabaseName": database,
            "TableName": table_name,
            "PartitionInput": partition,
        }
        if catalog_id:
            call["CatalogId"] = catalog_id
        try:
            glue.create_partition(**call)
        except Exception as already:
            # A partition the catalog already holds is one the write replaced the data of,
            # so its registration is brought up to date rather than failing the write.
            code = ""
            response = getattr(already, "response", None)
            if isinstance(response, dict):
                code = (response.get("Error") or {}).get("Code") or ""
            if code != "AlreadyExistsException":
                raise
            glue.update_partition(
                DatabaseName=database,
                TableName=table_name,
                PartitionValueList=list(values),
                PartitionInput=partition,
            )
    return None
"#;

/// The source of `awsglue/job.py`.
pub(super) const JOB_SOURCE: &str = r#"
"""The job one script run belongs to, as the script initialises and commits it."""

from awsglue import _bookmark


class Job(object):
    """`Job(glueContext)`: the run's own bookkeeping, as a script drives it."""

    def __init__(self, glue_context=None):
        self._glue_context = glue_context
        self._name = None
        self._arguments = {}
        self._committed = False

    def init(self, job_name, args=None):
        """"Initializes a job": the name it runs under and the arguments it was given."""
        self._name = str(job_name)
        self._arguments = dict(args or {})
        self._committed = False
        return None

    @property
    def isInitialized(self):
        return self._name is not None

    def is_initialized(self):
        return self._name is not None

    @property
    def name(self):
        return self._name

    def arguments(self):
        return dict(self._arguments)

    def commit(self):
        """"Commits the job": the point the script declares its work complete.

        This is also where the JOB BOOKMARK advances: "job.commit() … updates the state of
        the job bookmark", so the point each of this run's bookmarked reads reached becomes
        the point the next run of the job resumes from. A run the bookmark is paused or
        disabled for advances nothing ([`_bookmark.commit`]).
        """
        if self._name is None:
            raise RuntimeError("commit() is called on a job that was never initialised")
        _bookmark.commit()
        self._committed = True
        return None

    @property
    def isCommitted(self):
        return self._committed
"#;

/// The source of `awsglue/transforms.py`.
///
/// Each transform is the frame operation it names, applied the way the library applies one —
/// `Transform.apply(frame=…, …)` — so a script that calls a transform gets the frame that
/// operation computes. A transform this library does not supply is absent, so a script
/// reaching for one fails on the import rather than on a frame nothing transformed.
pub(super) const TRANSFORMS_SOURCE: &str = r#"
"""The transforms a Glue ETL script applies to a frame."""


class GlueTransform(object):
    """One transform, applied as the library applies one: `Transform.apply(...)`."""

    @classmethod
    def apply(cls, *args, **kwargs):
        return cls()(*args, **kwargs)

    @classmethod
    def name(cls):
        return cls.__name__


class ApplyMapping(GlueTransform):
    """The fields a mapping names, under the names and types it gives them."""

    def __call__(self, frame, mappings, case_sensitive=False, transformation_ctx="",
                 info="", stageThreshold=0, totalThreshold=0, **kwargs):
        return frame.apply_mapping(
            mappings,
            case_sensitive=case_sensitive,
            transformation_ctx=transformation_ctx,
            info=info,
        )


class SelectFields(GlueTransform):
    """Only the fields named."""

    def __call__(self, frame, paths, transformation_ctx="", info="", stageThreshold=0,
                 totalThreshold=0, **kwargs):
        return frame.select_fields(paths, transformation_ctx=transformation_ctx, info=info)


class DropFields(GlueTransform):
    """Every field except the ones named."""

    def __call__(self, frame, paths, transformation_ctx="", info="", stageThreshold=0,
                 totalThreshold=0, **kwargs):
        return frame.drop_fields(paths, transformation_ctx=transformation_ctx, info=info)


class RenameField(GlueTransform):
    """One field under a new name."""

    def __call__(self, frame, old_name, new_name, transformation_ctx="", info="",
                 stageThreshold=0, totalThreshold=0, **kwargs):
        return frame.rename_field(
            old_name, new_name, transformation_ctx=transformation_ctx, info=info
        )


class DropNullFields(GlueTransform):
    """The frame without the fields that hold no value in any record."""

    def __call__(self, frame, transformation_ctx="", info="", stageThreshold=0,
                 totalThreshold=0, **kwargs):
        records = frame.records
        empty = []
        for name, _declared in frame._columns:
            if all(record.get(name) is None for record in records):
                empty.append(name)
        if not empty:
            return frame
        return frame.drop_fields(empty, transformation_ctx=transformation_ctx, info=info)


class ResolveChoice(GlueTransform):
    """The frame with its ambiguous fields resolved."""

    def __call__(self, frame, specs=None, choice="", transformation_ctx="", info="",
                 stageThreshold=0, totalThreshold=0, **kwargs):
        return frame.resolveChoice(
            specs=specs, choice=choice, transformation_ctx=transformation_ctx, info=info
        )


class Filter(GlueTransform):
    """The records the predicate keeps."""

    def __call__(self, frame, f, transformation_ctx="", info="", stageThreshold=0,
                 totalThreshold=0, **kwargs):
        return frame.filter(f, transformation_ctx=transformation_ctx, info=info)


class Map(GlueTransform):
    """The records the function answered."""

    def __call__(self, frame, f, transformation_ctx="", info="", stageThreshold=0,
                 totalThreshold=0, **kwargs):
        return frame.map(f, transformation_ctx=transformation_ctx, info=info)


class SelectFromCollection(GlueTransform):
    """One frame of a collection of them."""

    def __call__(self, dfc, key, transformation_ctx="", **kwargs):
        return dfc.select(key, transformation_ctx=transformation_ctx)


__all__ = [
    "GlueTransform",
    "ApplyMapping",
    "SelectFields",
    "DropFields",
    "RenameField",
    "DropNullFields",
    "ResolveChoice",
    "Filter",
    "Map",
    "SelectFromCollection",
]
"#;
