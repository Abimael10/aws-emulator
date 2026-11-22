// SPDX-License-Identifier: CC0-1.0

//! `pyspark.sql`: the session a program starts and the dataset of rows it hands out.

/// The source of `pyspark/sql/__init__.py`.
pub(super) const SQL_SOURCE: &str = r#"
"""The Spark session of one application, and the rows a dataset of them holds."""

from pyspark import filesystem, parquet
from pyspark.conf import SparkConf
from pyspark.context import SparkContext
from pyspark.jvm import JavaSparkContext, SPARK_VERSION, gateway
from pyspark.rdd import RDD, slices

# Whether a write leaves the marker object beside its part files: the setting of the
# environment's OWN output committer, which is not the same for every environment that runs a
# Spark program ([`pyspark.release`]).
from pyspark.release import MARKS_SUCCESSFUL_JOBS

# The type system a schema is declared in lives where PySpark's own does, and `pyspark.sql`
# re-exports it, so a program that imports a type from either resolves it.
from pyspark.sql.types import (
    BIGINT,
    BINARY,
    BOOLEAN,
    DATE,
    DOUBLE,
    STRING,
    TIMESTAMP,
    VOID,
    ArrayType,
    BinaryType,
    BooleanType,
    ByteType,
    DataType,
    DateType,
    DecimalType,
    DoubleType,
    FloatType,
    IntegerType,
    LongType,
    MapType,
    NullType,
    Row,
    ShortType,
    StringType,
    StructField,
    StructType,
    TimestampNTZType,
    TimestampType,
    type_by_name,
)

# The failure a read that cannot be planned raises, which is the filesystem's one reading of
# that class.
AnalysisException = filesystem.AnalysisException


class SparkException(Exception):
    """A failure of the engine itself, as Spark raises one."""


# The column names a read gives the columns of a source that names none, as Spark names them.
UNNAMED_COLUMN = "_c%d"

# How a read treats a record it cannot read under the schema it was given, as the `mode`
# option names it: the default nulls the field it could not read, `DROPMALFORMED` drops the
# whole record, and `FAILFAST` fails the read. A mode outside these is one Spark reads as the
# default.
PARSE_PERMISSIVE = "permissive"
PARSE_DROPMALFORMED = "dropmalformed"
PARSE_FAILFAST = "failfast"
PARSE_MODES = (PARSE_PERMISSIVE, PARSE_DROPMALFORMED, PARSE_FAILFAST)

# What a read in the failing-fast mode says about the record it could not read: the fixed
# text of that failure, naming the RECORD it was reading, the mode in force, and the mode that
# would have read the record as a null instead.
MALFORMED_RECORD = (
    "Malformed records are detected in record parsing: [%s]. Parse Mode: FAILFAST. To "
    "process malformed records as null result, try setting the option 'mode' as 'PERMISSIVE'."
)

# The name the Hive layout gives the partition directory of a value that is not there, which
# is what a partition column holding no value is written under and read back from.
HIVE_NULL_PARTITION = "__HIVE_DEFAULT_PARTITION__"

# The expression surface a projection, a filter and a grouping are given, and the primitives
# for reading a value as a type and for folding a column's values into one: stated ONCE,
# where PySpark's own column expressions live, and used by both this engine and the function
# catalog over it ([`pyspark.sql.column`], [`pyspark.sql.functions`]).
from pyspark.sql.column import (
    AGGREGATES,
    CAST_TYPES,
    Column,
    aggregate as _aggregate_of,
    cast_value as _cast_value,
    field as _field,
    sort_key as _sort_key,
    type_of as _type_of,
    values_aggregate as _values_aggregate,
)


class _Read(object):
    """One SQL expression of a projection, as its own grammar reads it.

    `selectExpr` takes "SQL expressions" (`DataFrame.selectExpr`), and this is the reading of
    the ones this engine computes: a column, optionally cast, optionally inside an aggregate
    function, optionally named by an alias. It is read by walking the expression's own
    grammar — the function's parentheses, the `cast(<column> as <type>)` form and the trailing
    `as <alias>` — and never by matching the expression's text against a known one.
    """

    def __init__(self, function, column, cast, alias):
        self.function = function
        self.column = column
        self.cast = cast
        self.alias = alias


def _split_alias(text):
    """`text` and the alias a trailing `as <name>` gives it, or `None` when it names none.

    The `as` that names an alias is one OUTSIDE every bracket: the `cast(<column> as <type>)`
    form carries an `as` of its own, so the split is made at bracket depth zero and never
    inside a call. The LAST such `as` is the alias marker, as the expression's own grammar
    puts it at the end.
    """
    lowered = text.lower()
    depth = 0
    at = -1
    for index, character in enumerate(text):
        if character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
        elif depth == 0 and lowered[index : index + 4] == " as ":
            at = index
    if at < 0:
        return text.strip(), None
    alias = text[at + 4 :].strip().strip("`").strip('"')
    return text[:at].strip(), alias or None


def _call(text):
    """The function `text` calls and its argument, or `None` when it calls none."""
    if not text.endswith(")"):
        return None
    at = text.find("(")
    if at <= 0:
        return None
    return text[:at].strip().lower(), text[at + 1 : -1].strip()


def _read_expression(text):
    """[`_Read`] of one SQL expression."""
    body, alias = _split_alias(text.strip())
    function = None
    called = _call(body)
    if called is not None and called[0] in AGGREGATES:
        function, body = called
    cast = None
    called = _call(body)
    if called is not None and called[0] == "cast":
        inner, kind = _split_alias(called[1])
        if kind is None:
            raise AnalysisException("a cast names the type to read the value as: %s" % (text,))
        if kind.lower() not in CAST_TYPES:
            raise AnalysisException("cannot resolve the data type %s" % (kind,))
        cast = kind.lower()
        body = inner.strip()
    column = body.strip().strip("`")
    if alias is None:
        alias = text.strip()
    return _Read(function, column, cast, alias)


def read_sql_expression(text):
    """One SQL expression of a projection, as the COLUMN EXPRESSION it names.

    The grammar is the one `selectExpr` takes ([`_read_expression`]), so a form outside it
    raises that grammar's own analysis failure rather than answering an expression nothing
    computes.
    """
    read = _read_expression(text)
    if read.function and read.column in ("*", "1", ""):
        return _aggregate_of(read.function, None).alias(read.alias)
    held = _field(read.column)
    if read.cast is not None:
        held = held.cast(read.cast)
    if read.function:
        return _aggregate_of(read.function, held).alias(read.alias)
    return held.alias(read.alias)


def _range_aggregate(function, span):
    """The aggregate of an arithmetic progression, from the progression's own definition.

    The values of a `range` are an arithmetic progression, so its count, its bounds and its
    sum follow from its first value, its last and how many there are — exactly, in whole
    numbers, and without walking the progression. That is what lets an aggregate over a
    dataset of two hundred million values be answered at all: the answer is the one walking it
    would give, computed from the definition of the values rather than by adding them one by
    one, which is also why it is EXACT where a float accumulation would not be.
    """
    count = len(span)
    if function == "count":
        return count, BIGINT
    if count == 0:
        # Every aggregate of no rows is null, as Spark reports one.
        return None, BIGINT
    first = span[0]
    last = span[count - 1]
    if function == "first":
        return first, BIGINT
    if function == "last":
        return last, BIGINT
    # The product is always even: with an odd count the bounds sum to an even number, so the
    # halving below is exact in whole numbers whichever way round the progression runs.
    total = (first + last) * count // 2
    if function == "sum":
        return total, BIGINT
    if function == "min":
        return min(first, last), BIGINT
    if function == "max":
        return max(first, last), BIGINT
    if function in ("avg", "mean"):
        return float(total) / count, DOUBLE
    raise AnalysisException("Undefined function: %s" % (function,))


def _truth(value):
    """A reader option's value as the boolean Spark reads it as."""
    if isinstance(value, bool):
        return value
    return str(value).strip().lower() in ("true", "yes", "1")



def _typed(value):
    """One field as the value an inferred schema gives it."""
    text = value.strip()
    if not text:
        return None
    try:
        return int(text)
    except ValueError:
        pass
    try:
        return float(text)
    except ValueError:
        return value


def _hidden(path):
    """Whether a file is one a read of a directory passes over.

    The files whose name begins with `_` or `.` are the markers and the staging entries the
    Hadoop output committers leave (`_SUCCESS` among them), which is why a read of an output
    directory does not take them for data.
    """
    return path.rstrip("/").rsplit("/", 1)[-1][:1] in ("_", ".")


def _partition_values(path, held):
    """The partition columns the path of one file carries, by the Hive layout's spelling.

    A partitioned write holds the value of each partition column in a `<name>=<value>` path
    segment rather than in the files, so these are the columns a read of that directory
    recovers from the segments between the path it was asked for and the file itself. The
    value is read as the value its text spells, which is the inference Spark makes over a
    partition column, and the name Hive gives the partition of a null is read back as one.
    """
    root = filesystem._text(path).rstrip("/")
    if not held.startswith(root):
        return {}
    values = {}
    for segment in held[len(root) :].strip("/").split("/")[:-1]:
        if "=" not in segment:
            continue
        name, _marker, value = segment.partition("=")
        if name:
            held_value = filesystem.unescape_path_name(value)
            values[filesystem.unescape_path_name(name)] = (
                None if held_value == HIVE_NULL_PARTITION else _typed(held_value)
            )
    return values


def _partition_text(value):
    """One partition column's value, as the Hive layout spells it in a path.

    A boolean is spelled in the lower case the layout's own grammar writes it in, a value
    the row does not hold is the partition Hive names for the absent value, and every other
    value is the text it spells — through the layout's own escaping
    ([`pyspark.filesystem.escape_path_name`]), because the value is DATA: a cell spelling a
    path separator or a parent directory may not name an object outside the path the write
    was given.
    """
    if value is None:
        return HIVE_NULL_PARTITION
    if isinstance(value, bool):
        return "true" if value else "false"
    return filesystem.escape_path_name(str(value))


class DataFrameReader(object):
    """Where a dataset of rows is READ from: the objects its source path names.

    "Returns a DataFrameReader that can be used to read data in as a DataFrame"
    (`SparkSession.read`). A path naming one object reads that object and a path naming a
    prefix reads every object under it, which is how a Spark source addresses a directory of
    part files; the read is made through the filesystem, so it is made as the identity the
    program runs as and a refused one raises that refusal.
    """

    def __init__(self, session=None):
        self._session = session
        self._options = {}
        self._format = None
        self._columns = None
        self._declared = {}

    def format(self, source):
        self._format = str(source).strip().lower()
        return self

    def option(self, key, value):
        self._options[str(key).strip().lower()] = value
        return self

    def options(self, **named):
        for key, value in named.items():
            self.option(key, value)
        return self

    def schema(self, schema):
        """The schema the read is made under: the column names, and the types declared.

        A schema states what each column HOLDS, so a value a read takes in is read as the
        type its own column declares rather than as the text it arrived as — which is what
        makes a read under a schema answer the same values whatever format carried them. A
        schema given as names alone declares no types, and a value then stands as it came.
        """
        self._declared = {}
        if hasattr(schema, "fields"):
            self._columns = [field.name for field in schema.fields]
            for field in schema.fields:
                self._declared[field.name] = field.dataType.simpleString()
        elif hasattr(schema, "fieldNames"):
            self._columns = list(schema.fieldNames())
        elif isinstance(schema, (list, tuple)):
            self._columns = [str(column) for column in schema]
        elif isinstance(schema, str):
            # The data-definition spelling of a schema: `<name> <type>` per column.
            self._columns = []
            for field in schema.split(","):
                stated = field.strip().split()
                if not stated:
                    continue
                self._columns.append(stated[0])
                if len(stated) > 1:
                    self._declared[stated[0]] = " ".join(stated[1:]).lower()
        return self

    def _declared_value(self, name, value):
        """One value as the type its column declares holds it.

        A value the declared type cannot be read from is null, which is what a read in the
        permissive mode Spark reads in by default answers for a malformed one. A type this
        engine reads no value as leaves the value as it came.
        """
        kind = self._declared.get(name)
        if kind is None or kind not in CAST_TYPES:
            return value
        return _cast_value(value, kind)

    def _declared_types(self, columns):
        """The type each declared column carries, by the name a schema spells it."""
        held = {}
        for name in columns or []:
            kind = self._declared.get(name)
            if kind in CAST_TYPES:
                held[name] = CAST_TYPES[kind][0]
        return held

    def _read(self, kind, path):
        if path is None:
            raise AnalysisException("a read names no path")
        read = filesystem.source_objects(path)
        if kind in ("csv", "com.databricks.spark.csv"):
            return self._csv(read)
        if kind == "json":
            return self._json(read)
        if kind == "text":
            return self._text(read)
        if kind == "parquet":
            return self._parquet(read, path)
        raise AnalysisException(
            "This emulator reads a Spark source in the csv, json, text and parquet "
            "formats; %s names another one" % (kind,)
        )

    def _csv(self, read):
        import csv
        import io

        header = _truth(self._options.get("header", False))
        infer = _truth(self._options.get("inferschema", False))
        separator = str(self._options.get("sep", self._options.get("delimiter", ",")))
        mode = self._parse_mode()
        columns = list(self._columns) if self._columns else None
        rows = []
        for _held, body in read:
            fields = csv.reader(
                io.StringIO(body.decode("utf-8")), delimiter=separator or ","
            )
            lines = [line for line in fields if line]
            if not lines:
                continue
            start = 0
            if header:
                if columns is None:
                    columns = [str(name).strip() for name in lines[0]]
                start = 1
            for line in lines[start:]:
                values = [_typed(value) for value in line] if infer else list(line)
                named = columns or [
                    UNNAMED_COLUMN % (index,) for index in range(len(values))
                ]
                if columns is None:
                    columns = list(named)
                record = dict(zip(named, values))
                malformed = False
                for name in list(record):
                    held = self._declared_value(name, record[name])
                    if held is None and self._unreadable(name, record[name]):
                        malformed = True
                    record[name] = held
                if malformed:
                    # A record the schema cannot be read over is treated as the read's own
                    # mode says: the failing-fast mode fails the read naming the record it
                    # was reading and the mode in force, the dropping one leaves the record
                    # out, and the default one keeps it with the field it could not read as a
                    # null.
                    if mode == PARSE_FAILFAST:
                        raise SparkException(
                            MALFORMED_RECORD
                            % ((separator or ",").join(str(value) for value in line),)
                        )
                    if mode == PARSE_DROPMALFORMED:
                        continue
                rows.append(Row(**record))
        return DataFrame(rows, columns or [], 1, None, self._declared_types(columns))

    def _parse_mode(self):
        """How this read treats a record it cannot read, as its `mode` option names it."""
        held = str(self._options.get("mode", PARSE_PERMISSIVE)).strip().lower()
        if held not in PARSE_MODES:
            # A mode Spark does not know is one it reads in the default mode.
            return PARSE_PERMISSIVE
        return held

    def _unreadable(self, name, value):
        """Whether `value` is a value the type column `name` declares cannot be read from.

        A field the source holds NOTHING in is a null rather than a malformed one — that is
        what an empty cell means — so only a value that is there and cannot be read as the
        declared type makes the record a malformed one.
        """
        if name not in self._declared or self._declared[name] not in CAST_TYPES:
            return False
        if value is None:
            return False
        return str(value).strip() != ""

    def _json(self, read):
        import json

        rows = []
        columns = list(self._columns) if self._columns else []
        for _held, body in read:
            for line in body.decode("utf-8").splitlines():
                if not line.strip():
                    continue
                record = json.loads(line)
                if not isinstance(record, dict):
                    record = {UNNAMED_COLUMN % (0,): record}
                for name in record:
                    if name not in columns:
                        columns.append(name)
                rows.append(Row(**record))
        return DataFrame(rows, columns)

    def _text(self, read):
        rows = []
        for _held, body in read:
            for line in body.decode("utf-8").splitlines():
                rows.append(Row(**{"value": line}))
        return DataFrame(rows, ["value"])

    def _parquet(self, read, path):
        """The dataset the Parquet files under a path hold, partition columns and all.

        Each file is READ as the Parquet file it is ([`pyspark.parquet`]), so the rows are the
        rows it holds and each column carries the type its own schema states. A directory
        written out PARTITIONED holds its partition columns in the path rather than in the
        files, as the Hive layout puts them, so a read of such a directory recovers each one
        from the `<name>=<value>` segments under the path it was asked for — which is what
        makes a partitioned write read back as the dataset that was written.
        """
        columns = []
        types = {}
        rows = []
        for held, body in read:
            if _hidden(held):
                continue
            try:
                named, kinds, values = parquet.read_file(body)
            except parquet.ParquetError as error:
                raise AnalysisException("%s: %s" % (held, error))
            partitioned = _partition_values(path, held)
            for index, name in enumerate(named):
                if name not in columns:
                    columns.append(name)
                if name not in types:
                    types[name] = kinds[index]
            for name in partitioned:
                if name not in columns:
                    columns.append(name)
                if name not in types and partitioned[name] is not None:
                    types[name] = _type_of(partitioned[name])
            for row in values:
                record = dict(zip(named, row))
                record.update(partitioned)
                rows.append(Row(**record))
        for name in columns:
            if name not in types:
                types[name] = STRING
        return DataFrame(rows, columns, 1, None, types)

    def load(self, path=None, format=None, schema=None, **named):
        if schema is not None:
            self.schema(schema)
        self.options(**named)
        return self._read(format or self._format or "csv", path)

    def csv(self, path, schema=None, **named):
        if schema is not None:
            self.schema(schema)
        self.options(**named)
        return self._read("csv", path)

    def json(self, path, schema=None, **named):
        if schema is not None:
            self.schema(schema)
        self.options(**named)
        return self._read("json", path)

    def text(self, path, **named):
        self.options(**named)
        return self._read("text", path)

    def parquet(self, *paths, **named):
        self.options(**named)
        if not paths:
            raise AnalysisException("a read names no path")
        read = self._read("parquet", paths[0])
        for path in paths[1:]:
            read = read.union(self._read("parquet", path))
        return read


class DataFrameWriter(object):
    """Where a dataset of rows is WRITTEN: through the filesystem its path resolves in."""

    def __init__(self, frame):
        self._frame = frame
        self._mode = "errorifexists"
        self._format = None
        self._options = {}
        self._partitions = []

    def mode(self, saveMode):
        self._mode = str(saveMode).lower()
        return self

    def format(self, source):
        self._format = str(source).lower()
        return self

    def option(self, key, value):
        self._options[str(key).strip().lower()] = value
        return self

    def options(self, **named):
        for key, value in named.items():
            self.option(key, value)
        return self

    def partitionBy(self, *columns):
        """The columns the output is PARTITIONED by, as `partitionBy` names them."""
        named = []
        for column in columns:
            if isinstance(column, (list, tuple)):
                named.extend(str(held) for held in column)
            else:
                named.append(str(column))
        for column in named:
            if column not in self._frame.columns:
                raise AnalysisException(
                    "Partition column `%s` not found in schema %s"
                    % (column, self._frame.schema.simpleString())
                )
        self._partitions = named
        return self

    def _rendered(self, kind, rows):
        if kind == "json":
            import json

            return [
                json.dumps(row.asDict()) if isinstance(row, Row) else json.dumps(row)
                for row in rows
            ]
        return [",".join(str(value) for value in row) for row in rows]

    def _grouped(self):
        """The rows by the values of the partition columns, in the order they first appear.

        A write with no partition columns is the one group of every row, written at the path
        itself; one with them leaves a directory per combination of their values, which is
        the Hive layout a partitioned write lands.
        """
        rows = self._frame.collect()
        if not self._partitions:
            return [("", rows)]
        order = []
        grouped = {}
        for row in rows:
            within = "/".join(
                "%s=%s" % (filesystem.escape_path_name(column), _partition_text(row[column]))
                for column in self._partitions
            )
            if within not in grouped:
                grouped[within] = []
                order.append(within)
            grouped[within].append(row)
        return [(within, grouped[within]) for within in order]

    def _write(self, kind, path):
        if path is None:
            raise AnalysisException("a write names no path")
        prefix = filesystem._text(path).rstrip("/")
        # A partition column is held in the PATH of the output rather than in its files,
        # which is what the Hive layout a partitioned write lands means.
        held = [
            column for column in self._frame.columns if column not in self._partitions
        ]
        for within, rows in self._grouped():
            at = "%s/%s" % (prefix, within) if within else prefix
            if kind == "parquet":
                self._parquet_parts(at, held, rows)
            else:
                self._text_parts(kind, at, held, rows)
        if MARKS_SUCCESSFUL_JOBS:
            filesystem.write_text("%s/_SUCCESS" % (prefix,), "")
        return None

    def _text_parts(self, kind, at, held, rows):
        """One part file per PARTITION of the dataset, as text.

        That is the output layout a write leaves: `coalesce` and `repartition` are what a
        program names that count with, so a write of a dataset coalesced to one partition
        leaves one part file, and a write of one repartitioned leaves a part per partition
        the rows were spread over.
        """
        lines = self._rendered(kind, [_without(row, held) for row in rows])
        for index, part in enumerate(slices(lines, self._frame.partitions())):
            filesystem.write_text(
                "%s/part-%05d" % (at, index),
                "".join("%s\n" % (line,) for line in part),
            )

    def _parquet_parts(self, at, held, rows):
        """The same part files, each one an Apache Parquet file of its own rows.

        The bytes are the format's own ([`pyspark.parquet`]): the values themselves in the
        file's pages, and the row count, the schema and each column's bounds in its footer.
        The part carries the suffix of the codec its pages are stored under, which is how
        Spark names a Parquet part file.
        """
        declared = dict(self._frame.dtypes())
        compression = self._options.get("compression", "snappy")
        _codec, suffix = parquet.codec_of(compression)
        for index, part in enumerate(slices(rows, self._frame.partitions())):
            body = parquet.file_bytes(
                held,
                [[row[column] for column in held] for row in part],
                declared,
                compression,
            )
            filesystem.write_bytes("%s/part-%05d%s.parquet" % (at, index, suffix), body)

    def csv(self, path, **named):
        self.options(**named)
        return self._write("csv", path)

    def text(self, path, **named):
        self.options(**named)
        return self._write("text", path)

    def json(self, path, **named):
        self.options(**named)
        return self._write("json", path)

    def parquet(self, path, mode=None, partitionBy=None, compression=None, **named):
        if mode is not None:
            self.mode(mode)
        if partitionBy is not None:
            self.partitionBy(partitionBy)
        if compression is not None:
            self.option("compression", compression)
        self.options(**named)
        return self._write("parquet", path)

    def save(self, path=None, format=None, mode=None, partitionBy=None, **named):
        if format is not None:
            self.format(format)
        if mode is not None:
            self.mode(mode)
        if partitionBy is not None:
            self.partitionBy(partitionBy)
        self.options(**named)
        return self._write(self._format or "parquet", path)


def _without(row, held):
    """One row of the columns `held` names, in that order."""
    if list(row.fields()) == list(held):
        return row
    return Row(**dict((column, row[column]) for column in held))



class DataFrame(object):
    """A dataset of rows: the rows themselves, and the computations over them.

    A dataset the application named by its VALUES holds those rows. One named as a span of
    numbers (`SparkSession.range`) holds the span itself instead and makes its rows only when
    something asks for them: a span is a progression, so what an aggregate over it comes to
    follows from its definition ([`_range_aggregate`]) and a dataset far larger than this
    process could hold rows for is still one every aggregate answers for.
    """

    def __init__(self, rows, columns, numPartitions=1, span=None, types=None):
        self._span = span
        self._rows = None if span is not None else list(rows)
        self._columns = list(columns)
        self._partitions = max(int(numPartitions or 1), 1)
        self._types = dict(types or {})

    def _frame_rows(self):
        """The rows of this dataset, made from its span the first time one is asked for."""
        if self._rows is None:
            name = self._columns[0] if self._columns else "id"
            self._rows = [Row(**{name: value}) for value in self._span]
        return self._rows

    @property
    def columns(self):
        return list(self._columns)

    def partitions(self):
        """How many partitions this dataset holds, which is what a write of it leaves."""
        return self._partitions

    @property
    def schema(self):
        """This dataset's schema: one field per column, carrying that column's own type."""
        return StructType(
            [
                StructField(name, type_by_name(self._column_type(name)) or NullType())
                for name in self._columns
            ]
        )

    def dtypes(self):
        return [(name, self._column_type(name)) for name in self._columns]

    def printSchema(self):
        print(self.schema.simpleString())
        return None

    def _column_type(self, name):
        """The type the column `name` carries: the one it was built with, or the one the first
        value of it that is not null has."""
        if name in self._types:
            return self._types[name]
        if self._span is not None and self._columns and name == self._columns[0]:
            return BIGINT
        for row in self._frame_rows():
            value = row[name]
            if value is not None:
                return _type_of(value)
        return VOID

    @property
    def rdd(self):
        return RDD(slices(self._frame_rows(), self._partitions))

    def coalesce(self, numPartitions):
        """The same rows over FEWER partitions — a coalesce never raises the count."""
        asked = max(int(numPartitions), 1)
        return self._over(min(self._partitions, asked))

    def repartition(self, numPartitions=None, *columns):
        """The same rows over the partitions asked for, as many as were named."""
        if numPartitions is None or not isinstance(numPartitions, int):
            return self._over(self._partitions)
        return self._over(max(int(numPartitions), 1))

    def _over(self, partitions):
        """The same dataset over `partitions` partitions, span and all."""
        if self._span is not None:
            return DataFrame([], self._columns, partitions, self._span, self._types)
        return DataFrame(self._rows, self._columns, partitions, None, self._types)

    @property
    def write(self):
        return DataFrameWriter(self)

    def collect(self):
        return list(self._frame_rows())

    def union(self, other):
        """The rows of this dataset followed by the rows of `other`, as `union` takes it.

        A union is positional: the columns of the two datasets are matched by position, as
        Spark's own `union` matches them, so the answer carries this dataset's names.
        """
        rows = list(self._frame_rows())
        for row in other.collect():
            held = list(row)
            rows.append(Row(**dict(zip(self._columns, held))))
        types = dict(other._types)
        types.update(self._types)
        return DataFrame(rows, self._columns, self._partitions, None, types)

    def unionAll(self, other):
        return self.union(other)

    def unionByName(self, other, allowMissingColumns=False):
        """The same union, matching the two datasets' columns by NAME rather than position."""
        rows = list(self._frame_rows())
        for row in other.collect():
            record = row.asDict()
            missing = [name for name in record if name not in self._columns]
            if missing and not allowMissingColumns:
                raise AnalysisException(
                    "Cannot resolve column name %s among (%s)"
                    % (missing[0], ", ".join(self._columns))
                )
            rows.append(
                Row(**dict((name, record.get(name)) for name in self._columns))
            )
        types = dict(other._types)
        types.update(self._types)
        return DataFrame(rows, self._columns, self._partitions, None, types)

    def count(self):
        if self._span is not None:
            return len(self._span)
        return len(self._rows)

    def take(self, num):
        if self._span is not None:
            name = self._columns[0] if self._columns else "id"
            return [
                Row(**{name: value}) for value in self._span[: max(int(num), 0)]
            ]
        return self._rows[: max(int(num), 0)]

    def first(self):
        taken = self.take(1)
        if not taken:
            return None
        return taken[0]

    def toLocalIterator(self):
        return iter(self._frame_rows())

    def select(self, *columns):
        """The dataset the expressions project, as `select` takes them.

        An argument is either the NAME of a column of this dataset or a column expression
        over it ([`pyspark.sql.column`]), and what the projection answers is what those
        expressions COMPUTE over this dataset's own rows: one row per row of it, or the one
        row every aggregate answers when every expression is an aggregate.
        """
        held = [self._resolved(column) for column in _flattened(columns)]
        if not held:
            raise AnalysisException("a projection names no expression")
        folding = [column for column in held if column.folds()]
        if folding and len(folding) != len(held):
            raise AnalysisException(
                "grouping expressions sequence is empty, and %s is not an aggregate function"
                % (held[0]._name,)
            )
        names = [column._name for column in held]
        if folding:
            return self._folded(held)
        rows = [
            Row(**dict((column._name, column.of(row)) for column in held))
            for row in self._frame_rows()
        ]
        return DataFrame(rows, names, self._partitions, None, self._carried(held))

    def _resolved(self, column):
        """One argument of a projection as the expression it names.

        A NAME is the column of this dataset it names, and anything else is the expression
        it already is — which is how Spark reads the two forms `select` takes.
        """
        if isinstance(column, Column):
            return column
        name = str(column)
        if name not in self._columns:
            raise AnalysisException("cannot resolve the column %s" % (name,))
        return _field(name)

    def _carried(self, columns):
        """The types a projection carries over: those of the columns it refers to.

        An expression that is a bare reference projects the column it names, so the type
        that column already carries is the type of the projected one; anything else is
        typed by the values it computes.
        """
        held = {}
        for column in columns:
            if column._field is not None and column._field in self._columns:
                held[column._name] = self._column_type(column._field)
        return held

    def _folded(self, columns, rows=None):
        """The ONE row every aggregate answers, over `rows` or over this whole dataset."""
        values = {}
        kinds = {}
        held = self._frame_rows() if rows is None else rows
        for column in columns:
            if (
                rows is None
                and self._span is not None
                and column._inner is not None
                and column._inner._field is not None
                and column._inner._field == self._columns[0]
            ):
                # The span's own definition answers it, so no row of it is ever made.
                value, kind = _range_aggregate(column._aggregate, self._span)
            else:
                value, kind = column.over(held)
            values[column._name] = value
            kinds[column._name] = kind
        names = [column._name for column in columns]
        return DataFrame([Row(**values)], names, 1, None, kinds)

    def withColumn(self, colName, col):
        """The same dataset with the column `colName` holding what `col` computes."""
        held = self._resolved(col)
        name = str(colName)
        names = list(self._columns)
        if name not in names:
            names.append(name)
        rows = []
        for row in self._frame_rows():
            record = row.asDict()
            record[name] = held.of(row)
            rows.append(Row(**dict((key, record.get(key)) for key in names)))
        types = dict(self._types)
        if name in types:
            del types[name]
        carried = self._carried([held])
        if held._name in carried:
            types[name] = carried[held._name]
        return DataFrame(rows, names, self._partitions, None, types)

    def withColumnRenamed(self, existing, new):
        """The same dataset with one column under another name."""
        old = str(existing)
        if old not in self._columns:
            return self
        names = [str(new) if name == old else name for name in self._columns]
        rows = []
        for row in self._frame_rows():
            record = row.asDict()
            rows.append(
                Row(
                    **dict(
                        (
                            str(new) if name == old else name,
                            record.get(name),
                        )
                        for name in self._columns
                    )
                )
            )
        types = {}
        for name in self._columns:
            types[str(new) if name == old else name] = self._column_type(name)
        return DataFrame(rows, names, self._partitions, None, types)

    def drop(self, *columns):
        """The same dataset without the columns named."""
        dropped = [
            column._name if isinstance(column, Column) else str(column)
            for column in _flattened(columns)
        ]
        names = [name for name in self._columns if name not in dropped]
        rows = [
            Row(**dict((name, row[name]) for name in names))
            for row in self._frame_rows()
        ]
        types = dict((name, self._column_type(name)) for name in names)
        return DataFrame(rows, names, self._partitions, None, types)

    def filter(self, condition):
        """The rows of this dataset the condition holds for.

        The condition is a column expression over a row ([`pyspark.sql.column`]) or a
        function of one, which is what a filter of this engine applies to each row.
        """
        if isinstance(condition, Column):
            kept = [row for row in self._frame_rows() if condition.of(row)]
        elif callable(condition):
            kept = [row for row in self._frame_rows() if condition(row)]
        else:
            raise AnalysisException(
                "a filter takes a column expression or a function of a row; %r is neither"
                % (condition,)
            )
        return DataFrame(kept, self._columns, self._partitions, None, self._types)

    def where(self, condition):
        return self.filter(condition)

    def orderBy(self, *columns):
        """The same rows, ordered by the expressions named, each ascending or descending."""
        held = [self._resolved(column) for column in _flattened(columns)]
        rows = list(self._frame_rows())
        for column in reversed(held):
            rows = sorted(
                rows,
                key=lambda row, held=column: _sort_key(held.of(row)),
                reverse=column._descending,
            )
        return DataFrame(rows, self._columns, self._partitions, None, self._types)

    def sort(self, *columns):
        return self.orderBy(*columns)

    def distinct(self):
        """The rows of this dataset, each held once."""
        kept = []
        for row in self._frame_rows():
            if row not in kept:
                kept.append(row)
        return DataFrame(kept, self._columns, self._partitions, None, self._types)

    def limit(self, num):
        return DataFrame(
            self.take(num), self._columns, self._partitions, None, self._types
        )

    def groupBy(self, *columns):
        """The groups the rows fall into, by the expressions named."""
        return GroupedData(self, [self._resolved(column) for column in _flattened(columns)])

    def groupby(self, *columns):
        return self.groupBy(*columns)

    def agg(self, *exprs):
        """The aggregates over this whole dataset, which is the one group of every row."""
        return GroupedData(self, []).agg(*exprs)

    def toDF(self, *names):
        """The same rows under the column names given."""
        held = [str(name) for name in _flattened(names)]
        if not held:
            return self
        rows = [
            Row(**dict(zip(held, [row[name] for name in self._columns])))
            for row in self._frame_rows()
        ]
        types = {}
        for index, name in enumerate(held):
            if index < len(self._columns):
                types[name] = self._column_type(self._columns[index])
        return DataFrame(rows, held, self._partitions, None, types)

    def selectExpr(self, *exprs):
        """The dataset the SQL expressions project, as `selectExpr` takes them.

        Each expression is read by its own grammar ([`_read_expression`]) and then COMPUTED
        over this dataset: a projection answers one row per row of it, and an aggregate
        answers the one row every aggregate answers — over the span's own definition where
        this dataset is a span, and over its values otherwise.
        """
        reads = [_read_expression(str(expr)) for expr in exprs]
        if not reads:
            raise AnalysisException("a projection names no expression")
        aggregates = [read for read in reads if read.function]
        if aggregates and len(aggregates) != len(reads):
            raise AnalysisException(
                "grouping expressions sequence is empty, and %s is not an aggregate function"
                % (reads[0].alias,)
            )
        if aggregates:
            values = {}
            types = {}
            for read in reads:
                value, kind = self._aggregate(read)
                values[read.alias] = value
                types[read.alias] = kind
            names = [read.alias for read in reads]
            return DataFrame([Row(**values)], names, 1, None, types)
        names = [read.alias for read in reads]
        rows = []
        for row in self._frame_rows():
            projected = {}
            for read in reads:
                projected[read.alias] = _cast_value(self._value_of(row, read.column), read.cast)
            rows.append(Row(**projected))
        types = {}
        for read in reads:
            if read.cast is not None:
                types[read.alias] = CAST_TYPES[read.cast][0]
            else:
                types[read.alias] = self._column_type(read.column)
        return DataFrame(rows, names, self._partitions, None, types)

    def _value_of(self, row, column):
        if column not in self._columns:
            raise AnalysisException("cannot resolve the column %s" % (column,))
        return row[column]

    def _aggregate(self, read):
        """What one aggregate expression comes to over this dataset, and the type it has."""
        if read.function == "count" and read.column in ("*", "1", ""):
            return self.count(), BIGINT
        if read.column not in self._columns:
            raise AnalysisException("cannot resolve the column %s" % (read.column,))
        if self._span is not None and read.column == self._columns[0]:
            # The span's own definition answers it, so no row of it is ever made.
            value, kind = _range_aggregate(read.function, self._span)
        else:
            values = [
                _cast_value(row[read.column], read.cast) for row in self._frame_rows()
            ]
            value, kind = _values_aggregate(read.function, values)
            return value, kind
        if read.cast is None or value is None:
            return value, kind
        # A cast inside the aggregate reads every value as that type, so the aggregate's own
        # type is that one. The value is the aggregate of the span, read as the cast type:
        # the sum of a progression is exact in whole numbers, so reading it as a double is
        # ONE rounding of the true total — which is what a client comparing the two sees the
        # loss of, rather than the accumulation order of an engine it cannot observe.
        return _cast_value(value, read.cast), CAST_TYPES[read.cast][0]

    def show(self, n=20, truncate=True, vertical=False):
        for row in self.take(n):
            print(row)
        return None


def _flattened(columns):
    """The arguments of a call that takes several columns, lists and tuples of them too."""
    held = []
    for column in columns:
        if isinstance(column, (list, tuple)):
            held.extend(column)
        else:
            held.append(column)
    return held


class GroupedData(object):
    """The groups a dataset's rows fall into, and the aggregates over each of them.

    `groupBy` answers one of these: the keys are the expressions the rows are grouped by, and
    an aggregate over it answers ONE row per group — the key values, and what each aggregate
    folded that group's own rows into. A grouping by nothing at all is the one group of every
    row, which is what `DataFrame.agg` asks for.
    """

    def __init__(self, frame, keys):
        self._frame = frame
        self._keys = keys

    def _groups(self):
        """The (key values, rows) of each group, in the order the keys first appear."""
        if not self._keys:
            return [((), self._frame.collect())]
        order = []
        grouped = {}
        for row in self._frame.collect():
            values = tuple(key.of(row) for key in self._keys)
            if values not in grouped:
                grouped[values] = []
                order.append(values)
            grouped[values].append(row)
        return [(values, grouped[values]) for values in order]

    def agg(self, *exprs):
        """The dataset of one row per group, holding the keys and these aggregates."""
        columns = []
        for expr in _flattened(exprs):
            if isinstance(expr, dict):
                # The mapping form: the column to fold, by the function that folds it.
                for name in expr:
                    columns.append(_aggregate_of(str(expr[name]).lower(), _field(name)))
            else:
                columns.append(expr)
        if not columns:
            raise AnalysisException("an aggregation names no aggregate")
        for column in columns:
            if not isinstance(column, Column) or not column.folds():
                raise AnalysisException(
                    "%s is not an aggregate function" % (column,)
                )
        if not self._keys:
            return self._frame._folded(columns)
        names = [key._name for key in self._keys] + [
            column._name for column in columns
        ]
        rows = []
        types = {}
        for values, group in self._groups():
            record = {}
            for index, key in enumerate(self._keys):
                record[key._name] = values[index]
            for column in columns:
                value, kind = column.over(group)
                record[column._name] = value
                types[column._name] = kind
            rows.append(Row(**dict((name, record.get(name)) for name in names)))
        for key in self._keys:
            if key._field is not None:
                types[key._name] = self._frame._column_type(key._field)
        return DataFrame(rows, names, 1, None, types)

    def count(self):
        """How many rows each group holds, under the name Spark gives that column."""
        return self.agg(Column("count", None, "count", None))

    def sum(self, *columns):
        return self._over("sum", columns)

    def min(self, *columns):
        return self._over("min", columns)

    def max(self, *columns):
        return self._over("max", columns)

    def avg(self, *columns):
        return self._over("avg", columns)

    def mean(self, *columns):
        return self._over("avg", columns)

    def _over(self, function, columns):
        """One aggregate over the columns named, or over every numeric column.

        A grouped aggregate that names no column folds the NUMERIC columns of the dataset,
        which is what Spark's own `sum`, `min`, `max` and `avg` of a grouping do.
        """
        named = [str(column) for column in _flattened(columns)]
        if not named:
            named = [
                name
                for name in self._frame.columns
                if self._frame._column_type(name) in (BIGINT, DOUBLE)
                and name not in [key._name for key in self._keys]
            ]
        return self.agg([_aggregate_of(function, _field(name)) for name in named])


class SparkSession(object):
    """The session an application runs in, as `SparkSession.builder` hands one out."""

    class Builder(object):
        """The builder of one session: what it is named, where it runs, how configured."""

        def __init__(self):
            self._conf = SparkConf()

        def appName(self, name):
            self._conf.setAppName(name)
            return self

        def master(self, master):
            self._conf.setMaster(master)
            return self

        def config(self, key=None, value=None, conf=None, **named):
            if conf is not None:
                self._conf.setAll(conf.getAll())
            if key is not None:
                self._conf.set(key, value)
            for held, setting in named.items():
                self._conf.set(held, setting)
            return self

        def enableHiveSupport(self):
            return self.config("spark.sql.catalogImplementation", "hive")

        def getOrCreate(self):
            if SparkSession._active is None or SparkSession._active._stopped:
                SparkSession._active = SparkSession(
                    SparkContext.getOrCreate(self._conf)
                )
            return SparkSession._active

    _active = None

    def __init__(self, sparkContext):
        self._context = sparkContext
        self._jvm_gateway = gateway()
        self._java_context = JavaSparkContext(sparkContext.getConf())
        self._stopped = False
        SparkSession._active = self

    @property
    def sparkContext(self):
        return self._context

    @property
    def version(self):
        return SPARK_VERSION

    @property
    def conf(self):
        return self._context.getConf()

    @property
    def read(self):
        """Returns a DataFrameReader that can be used to read data in as a DataFrame."""
        return DataFrameReader(self)

    # The gateway and the Java context an application reaches the Hadoop filesystem
    # through, under the names PySpark gives them.
    @property
    def _jvm(self):
        return self._jvm_gateway

    @property
    def _jsc(self):
        return self._java_context

    @property
    def _sc(self):
        return self._context

    def createDataFrame(self, data, schema=None, **named):
        rows = list(data)
        columns = []
        if isinstance(schema, (list, tuple)):
            columns = [str(column) for column in schema]
        elif rows:
            first = rows[0]
            if isinstance(first, Row):
                columns = list(first._fields)
            elif isinstance(first, dict):
                columns = list(first.keys())
            else:
                columns = ["_%d" % (index + 1,) for index in range(len(first))]
        built = []
        for row in rows:
            if isinstance(row, Row):
                built.append(row)
            elif isinstance(row, dict):
                built.append(Row(**row))
            else:
                built.append(Row(**dict(zip(columns, list(row)))))
        return DataFrame(built, columns)

    def range(self, start, end=None, step=1, numPartitions=None):
        """The dataset of one bigint column over a span of numbers, as `range` names one.

        The span itself is what the dataset holds: its rows are made only when something asks
        for them, and an aggregate over it is answered from the span's own definition
        ([`DataFrame`]).
        """
        if end is None:
            start, end = 0, start
        span = range(int(start), int(end), int(step or 1))
        return DataFrame([], ["id"], numPartitions or 1, span, {"id": BIGINT})

    def stop(self):
        self._stopped = True
        self._context.stop()
        if SparkSession._active is self:
            SparkSession._active = None
        return None

    def newSession(self):
        return SparkSession(self._context)

    def __enter__(self):
        return self

    def __exit__(self, kind, value, traceback):
        self.stop()
        return False


# `SparkSession.builder`, the attribute an application starts from.
SparkSession.builder = SparkSession.Builder()

__all__ = [
    "AnalysisException",
    "ArrayType",
    "BinaryType",
    "BooleanType",
    "ByteType",
    "Column",
    "DataFrame",
    "DataFrameReader",
    "DataFrameWriter",
    "DataType",
    "DateType",
    "DecimalType",
    "DoubleType",
    "FloatType",
    "GroupedData",
    "IntegerType",
    "LongType",
    "MapType",
    "NullType",
    "Row",
    "ShortType",
    "SparkSession",
    "SparkException",
    "StringType",
    "StructField",
    "StructType",
    "TimestampNTZType",
    "TimestampType",
]
"#;
