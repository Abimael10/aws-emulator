// SPDX-License-Identifier: CC0-1.0

//! `pyspark.sql.types`: the data types a Spark program declares a schema in.
//!
//! The real PySpark distribution ships its type system as the `pyspark.sql.types` module and
//! re-exports it from `pyspark.sql`, so a program that imports a type from either resolves it.
//! That is what this is: ONE declaration of each type, named as Spark names it, with the
//! spelling each answers for `simpleString()` — the spelling a schema is read and printed in.
//!
//! Nothing here computes over data: these are the names a schema is built out of, and the
//! engine that holds the rows reads a column's type back through them ([`super::sql`]).

/// The source of `pyspark/sql/types.py`.
pub(super) const TYPES_SOURCE: &str = r#"
"""The data types a dataset's schema is declared in."""


# The spellings `DataType.simpleString()` answers for the types this engine's own columns
# carry, which is also how a schema reads and prints them.
BIGINT = "bigint"
DOUBLE = "double"
STRING = "string"
BOOLEAN = "boolean"
BINARY = "binary"
DATE = "date"
TIMESTAMP = "timestamp"
VOID = "void"


class DataType(object):
    """One type a column carries: what it is called, and how a schema spells it."""

    def typeName(self):
        """The type's own name, which is its class name without the `Type` suffix."""
        named = type(self).__name__
        if named.endswith("Type"):
            named = named[: -len("Type")]
        return named.lower()

    def simpleString(self):
        """How a schema spells this type, which is `typeName` for most of them."""
        return self.typeName()

    def jsonValue(self):
        return self.typeName()

    def json(self):
        import json

        return json.dumps(self.jsonValue(), separators=(",", ":"), sort_keys=True)

    def needConversion(self):
        return False

    def __eq__(self, other):
        if isinstance(other, DataType):
            return type(self) is type(other) and self.__dict__ == other.__dict__
        # A type also compares equal to the name a schema spells it by, which is how a
        # column's type is read back out of a frame's schema.
        if isinstance(other, str):
            return other in (self.simpleString(), self.typeName())
        return NotImplemented

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return equal
        return not equal

    def __hash__(self):
        return hash(self.simpleString())

    def __repr__(self):
        return "%s()" % (type(self).__name__,)


class NullType(DataType):
    """The type of a column holding nothing but nulls; `void` in a schema's spelling."""

    def typeName(self):
        return VOID


class StringType(DataType):
    """Text."""


class BinaryType(DataType):
    """A sequence of bytes."""


class BooleanType(DataType):
    """True or false."""


class DateType(DataType):
    """A calendar date, with no time of day."""


class TimestampType(DataType):
    """An instant, held to the microsecond."""


class TimestampNTZType(DataType):
    """An instant with no time zone, as Spark spells it."""

    def simpleString(self):
        return "timestamp_ntz"


class ByteType(DataType):
    """A whole number of one byte; `tinyint` in a schema's spelling."""

    def simpleString(self):
        return "tinyint"


class ShortType(DataType):
    """A whole number of two bytes; `smallint` in a schema's spelling."""

    def simpleString(self):
        return "smallint"


class IntegerType(DataType):
    """A whole number of four bytes; `int` in a schema's spelling."""

    def simpleString(self):
        return "int"


class LongType(DataType):
    """A whole number of eight bytes; `bigint` in a schema's spelling."""

    def simpleString(self):
        return BIGINT


class FloatType(DataType):
    """A number of four bytes."""


class DoubleType(DataType):
    """A number of eight bytes."""


class DecimalType(DataType):
    """An exact number of the given precision and scale."""

    def __init__(self, precision=10, scale=0):
        self.precision = int(precision)
        self.scale = int(scale)

    def simpleString(self):
        return "decimal(%d,%d)" % (self.precision, self.scale)

    def jsonValue(self):
        return self.simpleString()

    def __repr__(self):
        return "DecimalType(%d,%d)" % (self.precision, self.scale)


class ArrayType(DataType):
    """A sequence of values of one type."""

    def __init__(self, elementType, containsNull=True):
        self.elementType = elementType
        self.containsNull = bool(containsNull)

    def simpleString(self):
        return "array<%s>" % (self.elementType.simpleString(),)

    def __repr__(self):
        return "ArrayType(%r, %r)" % (self.elementType, self.containsNull)


class MapType(DataType):
    """Values of one type, each held under a key of another."""

    def __init__(self, keyType, valueType, valueContainsNull=True):
        self.keyType = keyType
        self.valueType = valueType
        self.valueContainsNull = bool(valueContainsNull)

    def simpleString(self):
        return "map<%s,%s>" % (
            self.keyType.simpleString(),
            self.valueType.simpleString(),
        )

    def __repr__(self):
        return "MapType(%r, %r, %r)" % (
            self.keyType,
            self.valueType,
            self.valueContainsNull,
        )


class StructField(DataType):
    """One field of a schema: its name, its type, and whether it admits nulls."""

    def __init__(self, name, dataType, nullable=True, metadata=None):
        self.name = str(name)
        self.dataType = dataType
        self.nullable = bool(nullable)
        self.metadata = dict(metadata or {})

    def simpleString(self):
        return "%s:%s" % (self.name, self.dataType.simpleString())

    def typeName(self):
        raise TypeError("a StructField is a field of a schema, not a type of its own")

    def jsonValue(self):
        return {
            "name": self.name,
            "type": self.dataType.jsonValue(),
            "nullable": self.nullable,
            "metadata": self.metadata,
        }

    def __repr__(self):
        return "StructField('%s', %s, %s)" % (
            self.name,
            type(self.dataType).__name__ + "()",
            self.nullable,
        )


class StructType(DataType):
    """A dataset's schema: its fields, in column order."""

    def __init__(self, fields=None):
        self.fields = list(fields or [])
        self.names = [field.name for field in self.fields]

    def add(self, field, data_type=None, nullable=True, metadata=None):
        """The same schema with one more field, as `StructType.add` takes one."""
        if isinstance(field, StructField):
            self.fields.append(field)
        else:
            if data_type is None:
                raise ValueError("a field added by name states the type it holds")
            self.fields.append(StructField(field, data_type, nullable, metadata))
        self.names = [held.name for held in self.fields]
        return self

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

    def typeName(self):
        return "struct"

    def simpleString(self):
        return "struct<%s>" % (",".join(field.simpleString() for field in self.fields),)

    def jsonValue(self):
        return {
            "type": "struct",
            "fields": [field.jsonValue() for field in self.fields],
        }

    def __repr__(self):
        return "StructType([%s])" % (", ".join(repr(field) for field in self.fields),)


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

    def __contains__(self, name):
        return name in self._fields

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


# Every type a schema of this engine names, by the spelling `simpleString()` answers for it
# and by the aliases Spark's own SQL accepts for the same type.
_BY_NAME = {
    "void": NullType,
    "null": NullType,
    "string": StringType,
    "str": StringType,
    "binary": BinaryType,
    "boolean": BooleanType,
    "bool": BooleanType,
    "date": DateType,
    "timestamp": TimestampType,
    "tinyint": ByteType,
    "byte": ByteType,
    "smallint": ShortType,
    "short": ShortType,
    "int": IntegerType,
    "integer": IntegerType,
    "bigint": LongType,
    "long": LongType,
    "float": FloatType,
    "real": FloatType,
    "double": DoubleType,
}


def type_by_name(name):
    """The type `name` spells, or None for a name no type of this engine answers to."""
    held = _BY_NAME.get(str(name).strip().lower())
    if held is None:
        return None
    return held()


__all__ = [
    "ArrayType",
    "BinaryType",
    "BooleanType",
    "ByteType",
    "DataType",
    "DateType",
    "DecimalType",
    "DoubleType",
    "FloatType",
    "IntegerType",
    "LongType",
    "MapType",
    "NullType",
    "Row",
    "ShortType",
    "StringType",
    "StructField",
    "StructType",
    "TimestampNTZType",
    "TimestampType",
]
"#;
