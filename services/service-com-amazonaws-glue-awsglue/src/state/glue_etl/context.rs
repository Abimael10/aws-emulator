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
/// files are Apache Parquet files any Parquet reader reads, stored under the codec the write
/// named. A sink asked to UPDATE THE CATALOG registers the partitions it wrote with the Data
/// Catalog's own API and brings the registered table's schema up to date with the one the
/// frame was written under — or, where the write asked for the difference to be logged
/// instead, leaves that schema as it stands — so a partition a run reports as written is one
/// the catalog answers for.
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
# an output that was written in full. A write APPENDS to the path it is given rather than
# replacing what is already there, so the index a part carries is the first one no part under
# that path carries yet ([`_part_name`]).
PART_PREFIX = "part-"
SUCCESS_MARKER = "_SUCCESS"

# The formats a frame is written in: the two text layouts, and the columnar one the Spark
# runtime under this library really encodes ([`pyspark.parquet`]). A layout outside these is
# one this emulator cannot write faithfully, and a sink asked for one says so rather than
# leaving behind an object that is not in that format.
TEXT_FORMATS = ("csv", "tsv", "text")
JSON_FORMATS = ("json", "jsonl")
PARQUET_FORMATS = ("parquet", "glueparquet")

# The connection options a sink HONOURS: where it writes, how it partitions what it writes,
# the codec the parts it writes are stored under, and whether and how what it wrote is
# brought into the Data Catalog.
HONOURED_OPTIONS = (
    "path",
    "paths",
    "partitionKeys",
    "compression",
    "enableUpdateCatalog",
    "updateBehavior",
)

# What a sink asked to update the catalog does to the TABLE it registers into, as
# `updateBehavior` names it: the schema the frame was written under replaces the registered
# one, or the difference is reported in the run's log and the registered schema left as it
# stands. A write naming neither updates the registered schema.
UPDATE_IN_DATABASE = "UPDATE_IN_DATABASE"
LOG_ONLY = "LOG"
UPDATE_BEHAVIOURS = (UPDATE_IN_DATABASE, LOG_ONLY)

# The members of a registered table a write of its schema carries back (`TableInput`): the
# table is updated by sending its own definition with the schema replaced, so everything else
# it states stays exactly as it stands.
TABLE_INPUT_MEMBERS = (
    "Name",
    "Description",
    "Owner",
    "LastAccessTime",
    "LastAnalyzedTime",
    "Retention",
    "StorageDescriptor",
    "PartitionKeys",
    "ViewOriginalText",
    "ViewExpandedText",
    "TableType",
    "Parameters",
    "TargetTable",
)

# How a `compression` option names no compression at all. A part of delimited text or of
# JSON documents is written as the characters it holds, so this is the only codec such a part
# is stored under; the columnar format's own codecs are the Spark runtime's to say
# ([`pyspark.parquet.codec_of`]).
UNCOMPRESSED_NAMES = ("", "none", "uncompressed")

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

    def _compression(self):
        """The codec the parts this sink writes are stored under, as either side names it.

        `compression` is a connection option of the write and a format option of the layout
        it writes in, and names the same codec in both; the layout's own is the more
        specific, so it decides when a write gives both. Read ONCE here and in one spelling —
        trimmed and folded, and a value naming nothing at all is a codec unnamed — so every
        layout's write reads the option the same way.
        """
        for named in (
            self._format_options.get("compression"),
            self._options.get("compression"),
        ):
            if named is None:
                continue
            held = str(named).strip().lower()
            if held:
                return held
        return None

    def _update_behaviour(self):
        """What a write asked to update the catalog does to the registered table's schema.

        A write naming no behaviour updates it, which is what a sink that only asks for the
        catalog to be updated gets; a value naming neither behaviour is refused rather than
        treated as one of them.
        """
        named = self._options.get("updateBehavior")
        if named is None or str(named).strip() == "":
            return UPDATE_IN_DATABASE
        held = str(named).strip().upper()
        if held not in UPDATE_BEHAVIOURS:
            raise ValueError(
                "a sink updates the catalog as one of %r; updateBehavior %r is neither"
                % (list(UPDATE_BEHAVIOURS), str(named))
            )
        return held

    def write(self, dynamic_frame, info=""):
        return self.writeFrame(dynamic_frame, info)

    def writeFrame(self, dynamic_frame, info=""):
        """Write the frame's records out, and register what was written if asked to."""
        if self._connection_type not in ("s3", "s3a", "s3n"):
            raise UnwritableFormat(
                "this emulator writes a frame to Amazon S3; %r is not a connection type it "
                "writes to" % (self._connection_type,)
            )
        # An option changes what the write DOES — the codec its parts are stored under, the
        # behaviour it updates the catalog with — so one this sink performs nothing for is
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
        # Both read before anything is written, so a write asked for a codec this emulator
        # cannot store a part under, or for an update behaviour that is neither of the two,
        # leaves no object behind at all.
        requested_compression = self._compression()
        behaviour = self._update_behaviour()
        if kind not in PARQUET_FORMATS:
            _refuse_text_codec(requested_compression)
        partition_keys = [
            str(key) for key in (self._options.get("partitionKeys") or []) if str(key)
        ]
        records = dynamic_frame.records
        columns = [name for name, _declared in dynamic_frame._columns]
        declared = dict(dynamic_frame._columns)
        # A value that is no value of its own column's DECLARED type is one no write can
        # store under that schema: a frame carries the value its source really held, and a
        # cell the declared type could not be read from is kept as the text it was, so
        # writing it would store text in a column the schema states as a number. Such a
        # column is resolved (`ResolveChoice`) before it is written, so the write is refused
        # naming it instead — before any object is written and any partition registered.
        unresolved = dynamic_frame._unresolved()
        if unresolved is not None:
            name, stated, value = unresolved
            raise ValueError(
                "the column %s is declared %s and the frame holds %r, which is no value of "
                "it; such a column is resolved before it is written" % (name, stated, value)
            )
        # The fields one part file HOLDS: a partition key's value is spelled by the path the
        # part is under, so it is not written into the part as well.
        held = [name for name in columns if name not in partition_keys]
        prefix = str(self._path).rstrip("/")
        written = []
        for values, group in _grouped(records, partition_keys):
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
            part = _part_name(at)
            if kind in PARQUET_FORMATS:
                # The part is the Parquet file its records make: the values themselves in
                # its pages, and the row count, the schema and each column's bounds in its
                # footer, encoded by the Spark runtime under this library.
                compression = requested_compression or "snappy"
                _codec, suffix = parquet.codec_of(compression)
                filesystem.write_bytes(
                    "%s/%s%s.parquet" % (at, part, suffix),
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
                    "%s/%s.%s" % (at, part, _suffix(kind)), body
                )
            if MARKS_SUCCESSFUL_JOBS:
                filesystem.write_text("%s/%s" % (at, SUCCESS_MARKER), "")
            written.append((values, at))
        if _table_data.truthy(self._options.get("enableUpdateCatalog")) and self._catalog_table:
            _register(
                self._catalog_database,
                self._catalog_table,
                self._catalog_id,
                partition_keys,
                written,
                [(name, declared.get(name)) for name in held],
                behaviour,
                self._glue_context.get_logger(),
            )
        return dynamic_frame


def _part_name(at):
    """The name this write gives the part it leaves under the path `at`.

    A write of a path APPENDS to it: the objects an earlier write left there are data of the
    same dataset, so a later write may not take their names — a run that wrote a partition
    once and writes it again leaves two parts, and the second does not replace the first.
    The index is therefore the first one no part already under that path carries, which for
    the first write of a path is the first index of all.

    Only the parts at that path's OWN level are counted: a listing is over the whole prefix
    ([`pyspark.filesystem.list_objects`]), and the parts under a sub-prefix of it belong to
    another directory of the dataset — a partition of its own — whose names this one cannot
    collide with.
    """
    taken = set()
    named = filesystem.prefix_of(at)
    prefix = "" if named is None else named[1].rstrip("/")
    if prefix:
        prefix = prefix + "/"
    for key, _size in filesystem.list_objects(at):
        rest = key[len(prefix):] if key.startswith(prefix) else key
        # A part under a SUB-prefix belongs to another directory of the dataset — a
        # partition of its own — whose names this one cannot collide with.
        if "/" in rest:
            continue
        if rest.startswith(PART_PREFIX):
            taken.add(rest.split(".")[0])
    index = 0
    while "%s%05d" % (PART_PREFIX, index) in taken:
        index += 1
    return "%s%05d" % (PART_PREFIX, index)


def _refuse_text_codec(compression):
    """Refuse a codec a part of delimited text or of JSON documents cannot be stored under.

    Such a part is written as the characters it holds, so the only codec this writer stores
    one under is none at all: a write asking for another is refused naming it, rather than
    leaving an uncompressed object behind under a name saying it was compressed.
    """
    named = str(compression or "").strip().lower()
    if named in UNCOMPRESSED_NAMES:
        return None
    raise UnwritableFormat(
        "this emulator writes delimited text and JSON documents uncompressed; the "
        "compression %r is not one it stores them under" % (named,)
    )


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


def _register(database, table_name, catalog_id, partition_keys, written, columns, behaviour,
              logger):
    """Register with the Data Catalog what a write left behind.

    "enableUpdateCatalog" asks for the write to be reflected in the catalog, and
    `updateBehavior` says what that does to the registered TABLE: its schema is brought up to
    date with the one the frame was written under, or the difference is reported in the run's
    log and the registered schema left as it stands. The partitions the write laid down are
    registered either way, which is what makes a partition a run reports as written one the
    catalog answers for.
    """
    # The Data Catalog client the RUNTIME supplies the guest, through this library's one
    # construction of it: its transport is this emulator's own in-process callback, signed
    # as the role the run is performed as — it resolves no endpoint, opens no connection and
    # reads no credential of the host.
    glue = _table_data.glue_client()
    request = {"DatabaseName": database, "Name": table_name}
    if catalog_id:
        request["CatalogId"] = catalog_id
    table = glue.get_table(**request)["Table"]
    descriptor = _schema_updated(glue, table, database, catalog_id, columns, behaviour, logger)
    if not partition_keys:
        return None
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


def _schema_updated(glue, table, database, catalog_id, columns, behaviour, logger):
    """Bring the registered table's schema up to date with the one the frame was written
    under, and answer the storage descriptor this write's partitions are registered under.

    A table already registered with the schema that was written has nothing to bring up to
    date, so nothing is written to the catalog for it.
    """
    descriptor = dict(table.get("StorageDescriptor") or {})
    wrote = [
        {"Name": str(name), "Type": str(declared or "string")} for name, declared in columns
    ]
    registered = [
        {
            "Name": str(column.get("Name") or ""),
            "Type": str(column.get("Type") or "string"),
        }
        for column in (descriptor.get("Columns") or [])
    ]
    if wrote == registered:
        return descriptor
    if behaviour == LOG_ONLY:
        logger.warn(
            "the frame written to %s.%s holds the columns %r, and the catalog holds %r for "
            "it; the registered schema is left as it stands"
            % (
                database,
                table.get("Name"),
                [column["Name"] for column in wrote],
                [column["Name"] for column in registered],
            )
        )
        return descriptor
    descriptor["Columns"] = wrote
    carried = dict(
        (member, table[member])
        for member in TABLE_INPUT_MEMBERS
        if table.get(member) is not None
    )
    carried["StorageDescriptor"] = descriptor
    call = {"DatabaseName": database, "TableInput": carried}
    if catalog_id:
        call["CatalogId"] = catalog_id
    glue.update_table(**call)
    return descriptor
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
