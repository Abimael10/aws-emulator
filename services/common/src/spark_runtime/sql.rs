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
    """A dataset of rows: the rows themselves, and the computations over them."""

    def __init__(self, rows, columns, numPartitions=1):
        self._rows = list(rows)
        self._columns = list(columns)
        self._partitions = max(int(numPartitions or 1), 1)

    @property
    def columns(self):
        return list(self._columns)

    def partitions(self):
        """How many partitions this dataset holds, which is what a write of it leaves."""
        return self._partitions

    @property
    def rdd(self):
        return RDD(slices(self._rows, self._partitions))

    def coalesce(self, numPartitions):
        """The same rows over FEWER partitions — a coalesce never raises the count."""
        asked = max(int(numPartitions), 1)
        return DataFrame(self._rows, self._columns, min(self._partitions, asked))

    def repartition(self, numPartitions=None, *columns):
        """The same rows over the partitions asked for, as many as were named."""
        if numPartitions is None or not isinstance(numPartitions, int):
            return DataFrame(self._rows, self._columns, self._partitions)
        return DataFrame(self._rows, self._columns, max(int(numPartitions), 1))

    @property
    def write(self):
        return DataFrameWriter(self)

    def collect(self):
        return list(self._rows)

    def count(self):
        return len(self._rows)

    def take(self, num):
        return self._rows[: max(int(num), 0)]

    def first(self):
        if not self._rows:
            return None
        return self._rows[0]

    def toLocalIterator(self):
        return iter(self._rows)

    def select(self, *columns):
        names = [str(column) for column in columns]
        for name in names:
            if name not in self._columns:
                raise ValueError("no such column: %s" % (name,))
        rows = [
            Row(**dict((name, row[name]) for name in names)) for row in self._rows
        ]
        return DataFrame(rows, names, self._partitions)

    def show(self, n=20, truncate=True, vertical=False):
        for row in self._rows[: max(int(n), 0)]:
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
        if end is None:
            start, end = 0, start
        rows = [Row(**{"id": value}) for value in range(start, end, step)]
        return DataFrame(rows, ["id"])

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

__all__ = ["DataFrame", "DataFrameReader", "DataFrameWriter", "Row", "SparkSession"]
"#;
