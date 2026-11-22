// SPDX-License-Identifier: CC0-1.0

//! `pyspark.sql.functions`: the catalog of functions a Spark program builds an expression
//! out of.
//!
//! The real PySpark distribution ships this module beside `pyspark.sql.types`, and a program
//! that computes over a DataFrame rather than over an RDD reaches for it by name
//! (`from pyspark.sql.functions import col, lit, sum`): every one of these answers a COLUMN
//! EXPRESSION ([`super::column`]), which a projection, a filter or a grouping then applies to
//! the program's own rows.
//!
//! What is here is the standard catalog this engine can really compute: the reference and the
//! literal, the text and arithmetic functions over one column, the conditional, and the
//! aggregates that fold a group into one value. A function outside it is one no expression is
//! made for, so the import of it fails the way an absent name fails rather than answering a
//! value nothing computed.

/// The source of `pyspark/sql/functions.py`.
pub(super) const FUNCTIONS_SOURCE: &str = r#"
"""The functions one column expression of a Spark program is built out of."""

from pyspark.sql.column import (
    AnalysisException,
    Column,
    When,
    aggregate,
    field,
    literal,
)

# The builtins this module's own functions compute with, held before the names that shadow
# them are bound: `sum`, `min`, `max`, `abs` and `round` are functions of this catalog too,
# exactly as they are of Spark's.
_abs = abs


def col(name):
    """A reference to the column `name` of a dataset."""
    return field(name)


def column(name):
    return field(name)


def lit(value):
    """A literal column: the same value for every row."""
    return literal(value)


def asc(col):
    return _as_column(col).asc()


def desc(col):
    return _as_column(col).desc()


def _as_column(value):
    """One argument of a function as the expression it names.

    A NAME handed to one of these functions is the column of that name — which is what
    `sum("amount")` means — while a value handed to one is the literal it spells.
    """
    if isinstance(value, Column):
        return value
    if isinstance(value, str):
        return field(value)
    return literal(value)


# --- the functions of one column ----------------------------------------------------------


def upper(col):
    held = _as_column(col)
    return Column("upper(%s)" % (held._name,), lambda row: _held_text(held, row, str.upper))


def lower(col):
    held = _as_column(col)
    return Column("lower(%s)" % (held._name,), lambda row: _held_text(held, row, str.lower))


def trim(col):
    held = _as_column(col)
    return Column("trim(%s)" % (held._name,), lambda row: _held_text(held, row, str.strip))


def ltrim(col):
    held = _as_column(col)
    return Column("ltrim(%s)" % (held._name,), lambda row: _held_text(held, row, str.lstrip))


def rtrim(col):
    held = _as_column(col)
    return Column("rtrim(%s)" % (held._name,), lambda row: _held_text(held, row, str.rstrip))


def _held_text(held, row, apply):
    value = held.of(row)
    return None if value is None else apply(str(value))


def length(col):
    held = _as_column(col)

    def read(row):
        value = held.of(row)
        return None if value is None else len(str(value))

    return Column("length(%s)" % (held._name,), read)


def abs(col):
    held = _as_column(col)

    def read(row):
        value = held.of(row)
        try:
            return None if value is None else _abs(value)
        except TypeError:
            return None

    return Column("abs(%s)" % (held._name,), read)


def round(col, scale=0):
    """The value rounded to `scale` decimal places, half away from zero.

    That is the rounding Spark's own `round` performs (HALF_UP), which differs from the
    half-to-even rounding Python's builtin does — so it is computed here rather than
    delegated to it.
    """
    held = _as_column(col)

    def read(row):
        import math

        value = held.of(row)
        if value is None:
            return None
        try:
            factor = 10.0 ** int(scale)
            scaled = float(value) * factor
        except (TypeError, ValueError, OverflowError):
            return None
        if scaled >= 0:
            scaled = math.floor(scaled + 0.5)
        else:
            scaled = math.ceil(scaled - 0.5)
        return scaled / factor

    return Column("round(%s, %d)" % (held._name, int(scale)), read)


def floor(col):
    held = _as_column(col)

    def read(row):
        import math

        value = held.of(row)
        try:
            return None if value is None else int(math.floor(float(value)))
        except (TypeError, ValueError):
            return None

    return Column("FLOOR(%s)" % (held._name,), read)


def ceil(col):
    held = _as_column(col)

    def read(row):
        import math

        value = held.of(row)
        try:
            return None if value is None else int(math.ceil(float(value)))
        except (TypeError, ValueError):
            return None

    return Column("CEIL(%s)" % (held._name,), read)


def sqrt(col):
    held = _as_column(col)

    def read(row):
        import math

        value = held.of(row)
        try:
            return None if value is None else math.sqrt(float(value))
        except (TypeError, ValueError):
            return None

    return Column("SQRT(%s)" % (held._name,), read)


def concat(*cols):
    """The text of every column, run together; null when any one of them is null."""
    held = [_as_column(each) for each in _flat(cols)]

    def read(row):
        values = [each.of(row) for each in held]
        if any(value is None for value in values):
            return None
        return "".join(str(value) for value in values)

    return Column("concat(%s)" % (", ".join(each._name for each in held),), read)


def concat_ws(sep, *cols):
    """The text of every column that has one, separated by `sep`."""
    held = [_as_column(each) for each in _flat(cols)]

    def read(row):
        return str(sep).join(
            str(value) for value in (each.of(row) for each in held) if value is not None
        )

    return Column(
        "concat_ws(%s, %s)" % (sep, ", ".join(each._name for each in held)), read
    )


def coalesce(*cols):
    """The first of these columns that is not null."""
    held = [_as_column(each) for each in _flat(cols)]

    def read(row):
        for each in held:
            value = each.of(row)
            if value is not None:
                return value
        return None

    return Column("coalesce(%s)" % (", ".join(each._name for each in held),), read)


def substring(str_col, pos, len_of):
    return _as_column(str_col).substr(pos, len_of)


def split(col, pattern, limit=-1):
    """The text of the column split on the regular expression `pattern`.

    `pattern` is a regular expression, as `split` takes one, and `limit` bounds how many
    parts the answer holds: a positive limit applies the pattern at most that many times
    less one, so the last part carries the rest, and a limit of zero or less applies it as
    many times as it matches. A pattern that is no regular expression is the analysis
    failure Spark raises for one.
    """
    import re

    held = _as_column(col)
    try:
        compiled = re.compile(str(pattern))
    except re.error as error:
        raise AnalysisException(
            "cannot resolve the pattern %s: %s" % (pattern, error)
        )
    parts = 0 if int(limit) <= 0 else int(limit) - 1

    def read(row):
        value = held.of(row)
        return None if value is None else compiled.split(str(value), parts)

    return Column("split(%s, %s, %d)" % (held._name, pattern, int(limit)), read)


def isnull(col):
    return _as_column(col).isNull()


def isnotnull(col):
    return _as_column(col).isNotNull()


def when(condition, value):
    """The conditional expression that answers `value` where `condition` holds."""
    return When([(_as_column(condition), _as_column(value))])


def expr(text):
    """One SQL expression, read by the engine's own reading of that grammar.

    The projection grammar `selectExpr` takes is the one this reads, so a function or a form
    outside it raises the analysis failure that grammar raises rather than answering an
    expression nothing computes.
    """
    from pyspark.sql import read_sql_expression

    return read_sql_expression(str(text))


# --- the aggregates ---------------------------------------------------------------------


def sum(col):
    return aggregate("sum", _as_column(col))


def avg(col):
    return aggregate("avg", _as_column(col))


def mean(col):
    return aggregate("avg", _as_column(col))


def min(col):
    return aggregate("min", _as_column(col))


def max(col):
    return aggregate("max", _as_column(col))


def first(col, ignorenulls=False):
    return aggregate("first", _as_column(col), False, ignorenulls)


def last(col, ignorenulls=False):
    return aggregate("last", _as_column(col), False, ignorenulls)


def count(col="*"):
    """How many rows the group holds — or how many of them the column has a value for."""
    if isinstance(col, str) and col.strip() in ("*", "1", ""):
        return aggregate("count", None)
    return aggregate("count", _as_column(col))


def countDistinct(col, *cols):
    """How many DISTINCT values the columns named hold, counted over all of them together.

    `count(DISTINCT a, b)` counts the distinct TUPLES of its expressions, and counts only
    the rows every one of them has a value for — so the columns are folded into one
    expression per row and that expression is what is counted.
    """
    held = [_as_column(each) for each in _flat((col,) + cols)]
    if len(held) == 1:
        return aggregate("count", held[0], True)

    def read(row):
        values = [each.of(row) for each in held]
        if any(value is None for value in values):
            return None
        return tuple(values)

    tupled = Column(", ".join(each._name for each in held), read)
    return aggregate("count", tupled, True)


def count_distinct(col, *cols):
    return countDistinct(col, *cols)


def sum_distinct(col):
    return aggregate("sum", _as_column(col), True)


def _flat(cols):
    """The arguments of a function that takes several columns, lists of them included."""
    held = []
    for each in cols:
        if isinstance(each, (list, tuple)):
            held.extend(each)
        else:
            held.append(each)
    return held


__all__ = [
    "abs",
    "asc",
    "avg",
    "ceil",
    "coalesce",
    "col",
    "column",
    "concat",
    "concat_ws",
    "count",
    "countDistinct",
    "count_distinct",
    "desc",
    "expr",
    "first",
    "floor",
    "isnotnull",
    "isnull",
    "last",
    "length",
    "lit",
    "lower",
    "ltrim",
    "max",
    "mean",
    "min",
    "round",
    "rtrim",
    "split",
    "sqrt",
    "substring",
    "sum",
    "sum_distinct",
    "trim",
    "upper",
    "when",
]
"#;
