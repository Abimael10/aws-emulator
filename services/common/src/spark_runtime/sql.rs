// SPDX-License-Identifier: CC0-1.0

//! `pyspark.sql`: the session a program starts and the dataset of rows it hands out.

/// The source of `pyspark/sql/__init__.py`.
pub(super) const SQL_SOURCE: &str = r#"
"""The Spark session of one application, and the rows a dataset of them holds."""

from pyspark import filesystem
from pyspark.conf import SparkConf
from pyspark.context import SparkContext
from pyspark.jvm import JavaSparkContext, SPARK_VERSION, gateway
from pyspark.rdd import RDD, slices

# The failure a read that cannot be planned raises, which is the filesystem's one reading of
# that class.
AnalysisException = filesystem.AnalysisException

# The column names a read gives the columns of a source that names none, as Spark names them.
UNNAMED_COLUMN = "_c%d"

# The names Spark gives the types a column carries, as `DataType.simpleString()` answers them.
BIGINT = "bigint"
DOUBLE = "double"
STRING = "string"
BOOLEAN = "boolean"
VOID = "void"

# What a `cast(<column> as <type>)` names a type by, mapped onto the name above and the
# function that reads a value as it. The aliases are the ones Spark's own SQL accepts.
CAST_TYPES = {
    "long": (BIGINT, int),
    "bigint": (BIGINT, int),
    "int": (BIGINT, int),
    "integer": (BIGINT, int),
    "short": (BIGINT, int),
    "smallint": (BIGINT, int),
    "byte": (BIGINT, int),
    "tinyint": (BIGINT, int),
    "double": (DOUBLE, float),
    "float": (DOUBLE, float),
    "real": (DOUBLE, float),
    "string": (STRING, str),
    "boolean": (BOOLEAN, bool),
}

# The aggregate functions an expression may name.
AGGREGATES = ("sum", "count", "min", "max", "avg", "mean")


def _type_of(value):
    """The type name Spark gives a value the Python type of this one."""
    if isinstance(value, bool):
        return BOOLEAN
    if isinstance(value, int):
        return BIGINT
    if isinstance(value, float):
        return DOUBLE
    if value is None:
        return VOID
    return STRING


class DataType(object):
    """One column's type, as a schema reports it."""

    def __init__(self, name):
        self._name = name

    def simpleString(self):
        return self._name

    def typeName(self):
        return self._name

    def __eq__(self, other):
        if isinstance(other, DataType):
            return self._name == other._name
        return self._name == other

    def __repr__(self):
        return "DataType(%s)" % (self._name,)


class StructField(object):
    """One field of a schema: its name and its type."""

    def __init__(self, name, dataType, nullable=True):
        self.name = name
        self.dataType = dataType
        self.nullable = nullable

    def simpleString(self):
        return "%s:%s" % (self.name, self.dataType.simpleString())

    def __repr__(self):
        return "StructField(%s,%s)" % (self.name, self.dataType.simpleString())


class StructType(object):
    """A dataset's schema: its fields, in column order."""

    def __init__(self, fields):
        self.fields = list(fields)

    def __getitem__(self, item):
        if isinstance(item, str):
            for field in self.fields:
                if field.name == item:
                    return field
            raise KeyError(item)
        return self.fields[item]

    def __iter__(self):
        return iter(self.fields)

    def __len__(self):
        return len(self.fields)

    def fieldNames(self):
        return [field.name for field in self.fields]

    def simpleString(self):
        return "struct<%s>" % (",".join(field.simpleString() for field in self.fields),)

    def __repr__(self):
        return "StructType(%r)" % (self.fields,)


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


def _values_aggregate(function, values):
    """The same aggregate over the values a dataset holds, folded with the builtin.

    A column whose values cannot be folded — text where a number is needed, or a mixture of
    the two — is a data-type mismatch, which is what the engine says rather than letting the
    builtin's own error out of the expression.
    """
    if function == "count":
        return len([value for value in values if value is not None]), BIGINT
    held = [value for value in values if value is not None]
    if not held:
        return None, BIGINT
    try:
        if function == "sum":
            total = sum(held)
            return total, _type_of(total)
        if function == "min":
            return min(held), _type_of(min(held))
        if function == "max":
            return max(held), _type_of(max(held))
        if function in ("avg", "mean"):
            return float(sum(held)) / len(held), DOUBLE
    except TypeError:
        raise AnalysisException(
            "cannot resolve %s due to data type mismatch" % (function,)
        )
    raise AnalysisException("Undefined function: %s" % (function,))


def _cast_value(value, cast):
    """`value` read as the type a cast names.

    A value the type cannot be read from is NULL, which is what a cast answers by default
    rather than ending the program: "the default (non-ANSI) mode returns null for a value it
    cannot cast". A boolean is read from the word a string spells rather than from whether the
    string is empty, so the text of `false` is false.
    """
    if value is None or cast is None:
        return value
    kind, reader = CAST_TYPES[cast]
    if kind == STRING:
        return str(value)
    if kind == BOOLEAN:
        if isinstance(value, bool):
            return value
        if isinstance(value, str):
            spelled = value.strip().lower()
            if spelled in ("true", "yes", "t", "y", "1"):
                return True
            if spelled in ("false", "no", "f", "n", "0"):
                return False
            return None
        return bool(value)
    try:
        return reader(value)
    except (TypeError, ValueError):
        return None


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


class Row(object):
    """One row: its fields by name and by position, as `Row` holds them."""

    def __init__(self, *values, **named):
        if values and named:
            raise ValueError("a Row is built from positions or from names, not both")
        if named:
            self._fields = list(named.keys())
            self._values = [named[field] for field in self._fields]
        else:
            self._fields = ["_%d" % (index + 1,) for index in range(len(values))]
            self._values = list(values)

    def __getitem__(self, item):
        if isinstance(item, str):
            return self._values[self._fields.index(item)]
        return self._values[item]

    def __getattr__(self, name):
        fields = object.__getattribute__(self, "_fields")
        if name in fields:
            return object.__getattribute__(self, "_values")[fields.index(name)]
        raise AttributeError("no such field: %s" % (name,))

    def __len__(self):
        return len(self._values)

    def __iter__(self):
        return iter(self._values)

    def __eq__(self, other):
        if isinstance(other, Row):
            return self._fields == other._fields and self._values == other._values
        return tuple(self._values) == other

    def __repr__(self):
        pairs = ", ".join(
            "%s=%r" % (field, value) for field, value in zip(self._fields, self._values)
        )
        return "Row(%s)" % (pairs,)

    def asDict(self, recursive=False):
        return dict(zip(self._fields, self._values))

    def fields(self):
        return list(self._fields)


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
        if hasattr(schema, "fieldNames"):
            self._columns = list(schema.fieldNames())
        elif isinstance(schema, (list, tuple)):
            self._columns = [str(column) for column in schema]
        elif isinstance(schema, str):
            self._columns = [
                field.strip().split(" ")[0] for field in schema.split(",") if field.strip()
            ]
        return self

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
        raise AnalysisException(
            "This emulator reads a Spark source in the csv, json and text formats; "
            "%s names another one" % (kind,)
        )

    def _csv(self, read):
        import csv
        import io

        header = _truth(self._options.get("header", False))
        infer = _truth(self._options.get("inferschema", False))
        separator = str(self._options.get("sep", self._options.get("delimiter", ",")))
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
                rows.append(Row(**dict(zip(named, values))))
        return DataFrame(rows, columns or [])

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
        raise AnalysisException(
            "This emulator reads a Spark source in the csv, json and text formats; "
            "parquet names another one"
        )


class DataFrameWriter(object):
    """Where a dataset of rows is WRITTEN: through the filesystem its path resolves in."""

    def __init__(self, frame):
        self._frame = frame
        self._mode = "errorifexists"
        self._format = None

    def mode(self, saveMode):
        self._mode = str(saveMode).lower()
        return self

    def format(self, source):
        self._format = str(source).lower()
        return self

    def option(self, key, value):
        return self

    def options(self, **named):
        return self

    def _rendered(self, kind):
        if kind == "json":
            import json

            return [
                json.dumps(row.asDict()) if isinstance(row, Row) else json.dumps(row)
                for row in self._frame.collect()
            ]
        return [
            ",".join(str(value) for value in row) for row in self._frame.collect()
        ]

    def _write(self, kind, path):
        lines = self._rendered(kind)
        prefix = filesystem._text(path).rstrip("/")
        # One part file per PARTITION of the dataset, which is the output layout a write
        # leaves: `coalesce` and `repartition` are what a program names that count with, so a
        # write of a dataset coalesced to one partition leaves one part file, and a write of
        # one repartitioned leaves a part per partition the rows were spread over.
        for index, part in enumerate(slices(lines, self._frame.partitions())):
            filesystem.write_text(
                "%s/part-%05d" % (prefix, index),
                "".join("%s\n" % (line,) for line in part),
            )
        filesystem.write_text("%s/_SUCCESS" % (prefix,), "")
        return None

    def csv(self, path, **named):
        return self._write("csv", path)

    def text(self, path, **named):
        return self._write("text", path)

    def json(self, path, **named):
        return self._write("json", path)

    def save(self, path=None, **named):
        return self._write(self._format or "csv", path)


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
                StructField(name, DataType(self._column_type(name)))
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
        names = [str(column) for column in columns]
        for name in names:
            if name not in self._columns:
                raise ValueError("no such column: %s" % (name,))
        rows = [
            Row(**dict((name, row[name]) for name in names))
            for row in self._frame_rows()
        ]
        types = dict((name, self._column_type(name)) for name in names)
        return DataFrame(rows, names, self._partitions, None, types)

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
    "DataFrame",
    "DataFrameReader",
    "DataFrameWriter",
    "DataType",
    "Row",
    "SparkSession",
    "StructField",
    "StructType",
]
"#;
