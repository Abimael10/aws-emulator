// SPDX-License-Identifier: CC0-1.0

//! `pyspark.sql.column`: one EXPRESSION over the rows of a dataset.
//!
//! A Spark program names a computation over its data by building a column expression — a
//! reference to a field, a literal, an operator over two of them, a function of one, an
//! aggregate that folds a whole group into one value — and hands it to a projection, a
//! filter or a grouping. That is what this module holds: the expression itself, which
//! ANSWERS what it says for a row handed to it, and the operators and comparisons that build
//! another out of it. The functions a program calls to make one live beside it
//! ([`super::functions`]), and the engine that holds the rows applies them ([`super::sql`]).
//!
//! Nothing is read off a program's source text: an expression is a value this module makes,
//! and asking it for its answer RUNS the program's own operators over the program's own
//! values. The null semantics are Spark's — an operator over a null answers null, and so
//! does one whose operands cannot be combined at all, which is what the non-strict mode
//! Spark reads by default answers.
//!
//! The primitives for reading a value AS a type, and for folding a column's values into one,
//! live here too, because both the expression surface and the SQL-expression surface of the
//! engine state them once.

/// The source of `pyspark/sql/column.py`.
pub(super) const COLUMN_SOURCE: &str = r#"
"""One expression over the rows of a dataset, and what it answers for a row."""

from pyspark import filesystem
from pyspark.sql.types import BIGINT, BOOLEAN, DOUBLE, STRING, VOID

# The failure an expression that cannot be resolved raises, which is the filesystem's one
# reading of that class.
AnalysisException = filesystem.AnalysisException

# What a `cast(<column> as <type>)` names a type by, mapped onto the name a schema spells
# that type and the function that reads a value as it. The aliases are the ones Spark's own
# SQL accepts.
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
AGGREGATES = ("sum", "count", "min", "max", "avg", "mean", "first", "last")


def type_of(value):
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


def cast_value(value, cast):
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


def values_aggregate(function, values, ignore_nulls=False):
    """What one aggregate comes to over the values handed to it, folded with the builtin.

    A column whose values cannot be folded — text where a number is needed, or a mixture of
    the two — is a data-type mismatch, which is what the engine says rather than letting the
    builtin's own error out of the expression.

    `ignore_nulls` is what `first` and `last` do with a null: they answer the first and the
    last VALUE of the group, null included, unless the caller asked for the first and last
    value that is not null. Every other aggregate is over the values a column has, which is
    what folding a null-free column means.
    """
    if function == "count":
        return len([value for value in values if value is not None]), BIGINT
    held = [value for value in values if value is not None]
    if function in ("first", "last"):
        taken = held if ignore_nulls else list(values)
        if not taken:
            return None, VOID
        value = taken[0] if function == "first" else taken[len(taken) - 1]
        return value, type_of(value)
    if not held:
        return None, BIGINT
    try:
        if function == "sum":
            total = sum(held)
            return total, type_of(total)
        if function == "min":
            return min(held), type_of(min(held))
        if function == "max":
            return max(held), type_of(max(held))
        if function in ("avg", "mean"):
            return float(sum(held)) / len(held), DOUBLE
    except TypeError:
        raise AnalysisException(
            "cannot resolve %s due to data type mismatch" % (function,)
        )
    raise AnalysisException("Undefined function: %s" % (function,))


def sort_key(value):
    """The key a sort compares one value on.

    A null sorts first, as Spark orders one by default; a number sorts among numbers and
    text among text, so a column holding both is ordered rather than refused.
    """
    if value is None:
        return (0, 0.0, "")
    if isinstance(value, bool):
        return (1, float(value), "")
    if isinstance(value, (int, float)):
        return (1, float(value), "")
    return (2, 0.0, str(value))


def _applied(apply):
    """An operator over two values, with Spark's null and mismatch semantics."""

    def held(left, right):
        if left is None or right is None:
            return None
        try:
            return apply(left, right)
        except (TypeError, ValueError, ZeroDivisionError):
            return None

    return held


def _literal_name(value):
    """What Spark calls a literal column, which is the literal itself as it is written."""
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return value
    return str(value)


class Column(object):
    """One expression: what it answers for a row, and what the projection calls it.

    An expression either READS a row (`of`) or FOLDS a group of them (`over`): an aggregate
    has no value for a single row, and a projection of one outside a grouping is the failure
    Spark raises for it.
    """

    def __init__(self, name, read=None, aggregate=None, inner=None, distinct=False,
                 descending=False, field=None, ignore_nulls=False):
        self._name = name
        self._read = read
        self._aggregate = aggregate
        self._inner = inner
        self._distinct = distinct
        self._descending = descending
        # What `first` and `last` do with a null: whether they pass over it or answer it.
        self._ignore_nulls = ignore_nulls
        # The column this expression is a bare REFERENCE to, where it is one: a projection
        # of a reference keeps the type that column already carries.
        self._field = field

    # --- what it answers ---------------------------------------------------------------

    def of(self, row):
        """What this expression answers for one row."""
        if self._read is None:
            raise AnalysisException(
                "grouping expressions sequence is empty, and %s is an aggregate function"
                % (self._name,)
            )
        return self._read(row)

    def folds(self):
        """Whether this expression is an aggregate over a group rather than a row."""
        return self._aggregate is not None

    def over(self, rows):
        """What this aggregate answers over `rows`, and the type that answer has."""
        if self._aggregate is None:
            raise AnalysisException("%s is not an aggregate function" % (self._name,))
        if self._inner is None:
            # `count(*)` counts the ROWS, nulls and all.
            return len(rows), BIGINT
        values = [self._inner.of(row) for row in rows]
        if self._distinct:
            held = []
            for value in values:
                if value not in held:
                    held.append(value)
            values = held
        return values_aggregate(self._aggregate, values, self._ignore_nulls)

    # --- naming ------------------------------------------------------------------------

    def alias(self, *names, **metadata):
        """The same expression under the name given, as `alias` names one."""
        if not names:
            return self
        return Column(
            str(names[0]),
            self._read,
            self._aggregate,
            self._inner,
            self._distinct,
            self._descending,
            self._field,
            self._ignore_nulls,
        )

    def name(self, *names):
        return self.alias(*names)

    def asc(self):
        return self._ordered(False)

    def desc(self):
        return self._ordered(True)

    def asc_nulls_first(self):
        return self._ordered(False)

    def desc_nulls_last(self):
        return self._ordered(True)

    def _ordered(self, descending):
        return Column(
            self._name,
            self._read,
            self._aggregate,
            self._inner,
            self._distinct,
            descending,
            self._field,
            self._ignore_nulls,
        )

    def __repr__(self):
        return "Column<'%s'>" % (self._name,)

    # --- operators ---------------------------------------------------------------------

    def _unary(self, spelled, apply):
        def read(row):
            held = self.of(row)
            if held is None:
                return None
            try:
                return apply(held)
            except (TypeError, ValueError):
                return None

        return Column("%s(%s)" % (spelled, self._name), read)

    def _binary(self, other, spelled, apply):
        right = as_column(other)
        applied = _applied(apply)

        def read(row):
            return applied(self.of(row), right.of(row))

        return Column("(%s %s %s)" % (self._name, spelled, right._name), read)

    def __add__(self, other):
        return self._binary(other, "+", lambda left, right: left + right)

    def __radd__(self, other):
        return as_column(other).__add__(self)

    def __sub__(self, other):
        return self._binary(other, "-", lambda left, right: left - right)

    def __rsub__(self, other):
        return as_column(other).__sub__(self)

    def __mul__(self, other):
        return self._binary(other, "*", lambda left, right: left * right)

    def __rmul__(self, other):
        return as_column(other).__mul__(self)

    def __truediv__(self, other):
        return self._binary(other, "/", lambda left, right: float(left) / float(right))

    def __rtruediv__(self, other):
        return as_column(other).__truediv__(self)

    def __mod__(self, other):
        return self._binary(other, "%", lambda left, right: left % right)

    def __neg__(self):
        return self._unary("negative", lambda held: -held)

    def __eq__(self, other):
        return self._binary(other, "=", lambda left, right: left == right)

    def __ne__(self, other):
        return self._binary(other, "!=", lambda left, right: left != right)

    def __lt__(self, other):
        return self._binary(other, "<", lambda left, right: left < right)

    def __le__(self, other):
        return self._binary(other, "<=", lambda left, right: left <= right)

    def __gt__(self, other):
        return self._binary(other, ">", lambda left, right: left > right)

    def __ge__(self, other):
        return self._binary(other, ">=", lambda left, right: left >= right)

    def __and__(self, other):
        right = as_column(other)

        def read(row):
            # Spark's logic is three-valued: a null operand makes the answer null unless the
            # other operand decides it on its own.
            left = self.of(row)
            held = right.of(row)
            if left is False or held is False:
                return False
            if left is None or held is None:
                return None
            return bool(left) and bool(held)

        return Column("(%s AND %s)" % (self._name, right._name), read)

    def __or__(self, other):
        right = as_column(other)

        def read(row):
            left = self.of(row)
            held = right.of(row)
            if left is True or held is True:
                return True
            if left is None or held is None:
                return None
            return bool(left) or bool(held)

        return Column("(%s OR %s)" % (self._name, right._name), read)

    def __invert__(self):
        def read(row):
            held = self.of(row)
            return None if held is None else not held

        return Column("(NOT %s)" % (self._name,), read)

    def isNull(self):
        return Column("(%s IS NULL)" % (self._name,), lambda row: self.of(row) is None)

    def isNotNull(self):
        return Column(
            "(%s IS NOT NULL)" % (self._name,), lambda row: self.of(row) is not None
        )

    def isin(self, *values):
        held = []
        for value in values:
            if isinstance(value, (list, tuple, set)):
                held.extend(value)
            else:
                held.append(value)
        return Column(
            "(%s IN (%s))" % (self._name, ", ".join(_literal_name(v) for v in held)),
            lambda row: self.of(row) in held,
        )

    def between(self, lowerBound, upperBound):
        lower = as_column(lowerBound)
        upper = as_column(upperBound)

        def read(row):
            held = self.of(row)
            if held is None:
                return None
            try:
                return lower.of(row) <= held <= upper.of(row)
            except TypeError:
                return None

        return Column(
            "((%s >= %s) AND (%s <= %s))"
            % (self._name, lower._name, self._name, upper._name),
            read,
        )

    def cast(self, dataType):
        """The same values read as the type named, as `cast` names one."""
        kind = dataType
        if hasattr(kind, "simpleString"):
            kind = kind.simpleString()
        kind = str(kind).strip().lower()
        if kind not in CAST_TYPES:
            raise AnalysisException("cannot resolve the data type %s" % (kind,))
        return Column(
            "CAST(%s AS %s)" % (self._name, kind.upper()),
            lambda row: cast_value(self.of(row), kind),
        )

    def astype(self, dataType):
        return self.cast(dataType)

    def contains(self, other):
        return self._binary(other, "contains", lambda left, right: str(right) in str(left))

    def startswith(self, other):
        return self._binary(
            other, "startswith", lambda left, right: str(left).startswith(str(right))
        )

    def endswith(self, other):
        return self._binary(
            other, "endswith", lambda left, right: str(left).endswith(str(right))
        )

    def substr(self, startPos, length):
        """The substring from `startPos`, one-based, as Spark counts the position."""

        def read(row):
            held = self.of(row)
            if held is None:
                return None
            at = max(int(startPos) - 1, 0)
            return str(held)[at : at + int(length)]

        return Column(
            "substring(%s, %s, %s)" % (self._name, startPos, length), read
        )


class When(Column):
    """A conditional expression: the first branch whose condition holds, else the default."""

    def __init__(self, branches, default=None):
        self._branches = list(branches)
        self._default = default
        Column.__init__(self, _when_name(self._branches, default), self._branch_value)

    def _branch_value(self, row):
        for condition, value in self._branches:
            if condition.of(row):
                return value.of(row)
        return None if self._default is None else self._default.of(row)

    def when(self, condition, value):
        return When(self._branches + [(as_column(condition), as_column(value))], self._default)

    def otherwise(self, value):
        return When(self._branches, as_column(value))


def _when_name(branches, default):
    """What Spark calls a conditional expression, in its own `CASE WHEN` spelling."""
    spelled = " ".join(
        "WHEN %s THEN %s" % (condition._name, value._name)
        for condition, value in branches
    )
    if default is not None:
        spelled = "%s ELSE %s" % (spelled, default._name)
    return "CASE %s END" % (spelled,)


def field(name):
    """A reference to the field `name` of a row."""
    held = str(name)

    def read(row):
        try:
            return row[held]
        except (KeyError, ValueError, IndexError):
            raise AnalysisException("cannot resolve the column %s" % (held,))

    return Column(held, read, None, None, False, False, held)


def literal(value):
    """A literal: the same value for every row."""
    return Column(_literal_name(value), lambda row: value)


def as_column(value):
    """One argument of an expression as the expression it names.

    A column is itself, and anything else is the literal it spells — which is how Spark
    reads a plain value handed to an operator. A NAME is only a column where the engine
    resolves it against a dataset's own columns ([`super::sql`]), so a string here is the
    literal it is.
    """
    if isinstance(value, Column):
        return value
    return literal(value)


def aggregate(function, column, distinct=False, ignore_nulls=False):
    """The aggregate `function` over the expression `column`, named as Spark names it.

    `ignore_nulls` is what `first` and `last` do with a null: they answer the first and the
    last value of the group, null included, unless the caller asks for the first and last
    that is not null. Every other aggregate folds the values a column has.
    """
    if function not in AGGREGATES:
        raise AnalysisException("Undefined function: %s" % (function,))
    if column is None:
        return Column("count(1)", None, "count", None)
    inner = column if isinstance(column, Column) else field(column)
    spelled = "%s(%s%s)" % (function, "DISTINCT " if distinct else "", inner._name)
    return Column(spelled, None, function, inner, distinct, False, None, ignore_nulls)


__all__ = [
    "AGGREGATES",
    "CAST_TYPES",
    "AnalysisException",
    "Column",
    "When",
    "aggregate",
    "as_column",
    "cast_value",
    "field",
    "literal",
    "sort_key",
    "type_of",
    "values_aggregate",
]
"#;
