// SPDX-License-Identifier: CC0-1.0

//! `pyspark.sql.expression`: the SQL EXPRESSION GRAMMAR a projection's expressions are read
//! by.
//!
//! `DataFrame.selectExpr` takes "SQL expressions" and `functions.expr` takes one, so what a
//! program hands either is a SQL expression — a literal, a reference to a column, a call of a
//! function, a cast, an operator over two of them, a conditional — and what it names is the
//! COLUMN EXPRESSION that computes it ([`super::column`]). This module is that reading: a
//! tokenizer over the expression's own lexical forms and a recursive descent over its own
//! precedence, building the expression as it goes. Nothing is matched against a known
//! expression and no text is scanned for a shape: an operator is read where the grammar puts
//! one, and a function is the one the standard catalog ([`super::functions`]) holds under the
//! name the call spells.
//!
//! A name no catalog holds is the undefined-function failure Spark raises for it, and a form
//! outside the grammar is that grammar's own analysis failure — never an expression nothing
//! computes.
//!
//! # Bounds
//!
//! The expression is caller-supplied text, so how long one may be, how many tokens it lexes
//! into and how deeply it may nest are bounded here; past them the reading raises rather than
//! spending this process on one expression. Every production that recurses on the expression's
//! own text accounts for a level against that nesting bound, so a chain of prefix operators is
//! answered by it too — and because one level of nesting costs several frames, an expression
//! that reaches the INTERPRETER's own recursion limit first is answered by the same bound
//! rather than by an error of the interpreter ([`read_sql_expression`]). The pattern of a
//! `LIKE` is matched by a walk over the pattern's own units rather than by a regular
//! expression, so what one row costs is the product of the two lengths and no pattern can make
//! the matching super-linear. The interpreter's own step and memory ceilings bound the reading
//! besides, in host code no program can reach.

/// The source of `pyspark/sql/expression.py`.
pub(super) const EXPRESSION_SOURCE: &str = r#"
"""The SQL expression grammar `selectExpr` and `expr` read one expression by."""

from pyspark.sql.column import (
    AGGREGATES,
    AnalysisException,
    Column,
    When,
    aggregate,
    field,
    literal,
)
import pyspark.sql.functions as functions

# How long an expression this reading takes, how many tokens it lexes out of one, and how
# deeply one may nest. CHOSEN: bounds on what reading ONE expression costs this process, far
# above any expression a program writes.
MAX_EXPRESSION_CHARACTERS = 4096
MAX_EXPRESSION_TOKENS = 1024
MAX_EXPRESSION_DEPTH = 32

# How many distinct `LIKE` patterns one expression keeps the reading of. CHOSEN: a pattern is
# the same for every row of a dataset, so one is almost always enough; what this bounds is a
# pattern computed per row, whose readings are then not kept without limit.
MAX_LIKE_PATTERNS = 16

# The words of the grammar itself, which therefore name neither a column nor an alias.
KEYWORDS = (
    "and",
    "as",
    "between",
    "case",
    "cast",
    "distinct",
    "else",
    "end",
    "false",
    "in",
    "is",
    "like",
    "not",
    "null",
    "or",
    "then",
    "true",
    "when",
)

# The operators the grammar spells, longest first so a two-character one is lexed whole.
OPERATORS = (
    "<=",
    ">=",
    "<>",
    "!=",
    "==",
    "||",
    "=",
    "<",
    ">",
    "+",
    "-",
    "*",
    "/",
    "%",
    "(",
    ")",
    ",",
)

# The functions a call may name: the standard catalog of `pyspark.sql.functions`, by the
# lower-case spelling a SQL expression names one under. That module's own `__all__` is the
# ONE statement of which functions this runtime computes, so nothing is listed twice here.
CATALOG = dict((name.lower(), name) for name in functions.__all__)


def _quoted(text, index, mark):
    """The quoted value that starts at `index`, and where it ends.

    A doubled quotation mark inside one stands for the mark itself, which is how SQL spells
    one, and a backslash escapes the character after it, which is the escaping Spark reads by
    default.
    """
    index += 1
    held = []
    size = len(text)
    while index < size:
        character = text[index]
        if character == "\\" and index + 1 < size:
            held.append(text[index + 1])
            index += 2
            continue
        if character == mark:
            if index + 1 < size and text[index + 1] == mark:
                held.append(mark)
                index += 2
                continue
            return "".join(held), index + 1
        held.append(character)
        index += 1
    raise AnalysisException("the expression %s ends inside a quoted value" % (text,))


def _number(text, index):
    """The number that starts at `index`, as the value it spells, and where it ends."""
    at = index
    size = len(text)
    point = False
    exponent = False
    while index < size:
        character = text[index]
        if character.isdigit():
            index += 1
            continue
        if character == "." and not point and not exponent:
            point = True
            index += 1
            continue
        if (
            character in "eE"
            and not exponent
            and index + 1 < size
            and (text[index + 1].isdigit() or text[index + 1] in "+-")
        ):
            exponent = True
            index += 2
            continue
        break
    spelled = text[at:index]
    if point or exponent:
        return float(spelled), index
    return int(spelled), index


def _tokens(text):
    """The tokens of one expression: `(kind, value)` pairs, where kind is one of
    `name`, `string`, `number` and `operator`."""
    held = []
    index = 0
    size = len(text)
    while index < size:
        if len(held) >= MAX_EXPRESSION_TOKENS:
            raise AnalysisException("the expression %s holds too many tokens" % (text,))
        character = text[index]
        if character in " \t\r\n":
            index += 1
            continue
        if character in ("'", '"'):
            value, index = _quoted(text, index, character)
            held.append(("string", value))
            continue
        if character == "`":
            value, index = _quoted(text, index, "`")
            held.append(("name", value))
            continue
        if character.isdigit() or (
            character == "." and index + 1 < size and text[index + 1].isdigit()
        ):
            value, index = _number(text, index)
            held.append(("number", value))
            continue
        if character.isalpha() or character == "_":
            at = index
            while index < size and (text[index].isalnum() or text[index] in "_$"):
                index += 1
            # A qualified name (`<relation>.<column>`) is ONE name, as a projection spells a
            # column of a named relation.
            while (
                index + 1 < size
                and text[index] == "."
                and (text[index + 1].isalnum() or text[index + 1] == "_")
            ):
                index += 1
                while index < size and (text[index].isalnum() or text[index] in "_$"):
                    index += 1
            held.append(("name", text[at:index]))
            continue
        spelled = None
        for operator in OPERATORS:
            if text.startswith(operator, index):
                spelled = operator
                break
        if spelled is None:
            raise AnalysisException(
                "cannot read the expression %s: %s is not part of it" % (text, character)
            )
        held.append(("operator", spelled))
        index += len(spelled)
    return held


def _compared(left, spelled, right):
    """The comparison the operator `spelled` names, over the two expressions."""
    if spelled in ("=", "=="):
        return left == right
    if spelled in ("<>", "!="):
        return left != right
    if spelled == "<":
        return left < right
    if spelled == "<=":
        return left <= right
    if spelled == ">":
        return left > right
    return left >= right


def _within(held, values):
    """`IN`: whether the expression's value is one of the values listed."""

    def read(row):
        left = held.of(row)
        if left is None:
            return None
        return left in [each.of(row) for each in values]

    return Column(
        "(%s IN (%s))" % (held._name, ", ".join(each._name for each in values)), read
    )


def _like_pattern(spelled):
    """The UNITS a `LIKE` pattern is made of, as `(kind, value)` pairs.

    `%` stands for any run of characters and `_` for any one of them, as `LIKE` defines them;
    every other character stands for itself, and a backslash escapes the one after it.
    """
    units = []
    index = 0
    while index < len(spelled):
        character = spelled[index]
        if character == "\\" and index + 1 < len(spelled):
            units.append(("one_of", spelled[index + 1]))
            index += 2
            continue
        if character == "%":
            # A run of wildcards stands for the same thing one of them does, so the units
            # hold one: what the match below costs follows from the pattern's LENGTH and
            # never from how many wildcards a caller wrote.
            if not units or units[len(units) - 1][0] != "any":
                units.append(("any", ""))
        elif character == "_":
            units.append(("any_one", ""))
        else:
            units.append(("one_of", character))
        index += 1
    return units


def _like_matches(text, units):
    """Whether `text` matches the units of a `LIKE` pattern.

    A walk over the two, remembering the LAST wildcard and where the text stood at it: a
    mismatch after one goes back to that wildcard and gives it one more character, so the walk
    spends at most the product of the two lengths and no pattern a caller writes can make it
    cost more than that. This is why the match is not a regular expression: a translated
    pattern of several wildcards is where a backtracking engine explores combinations of them
    super-linearly in the length of the text.
    """
    held = len(units)
    at_text = 0
    at_unit = 0
    wildcard = -1
    after_wildcard = 0
    while at_text < len(text):
        if at_unit < held and units[at_unit][0] == "any":
            wildcard = at_unit
            after_wildcard = at_text
            at_unit += 1
            continue
        if at_unit < held and (
            units[at_unit][0] == "any_one" or units[at_unit][1] == text[at_text]
        ):
            at_unit += 1
            at_text += 1
            continue
        if wildcard < 0:
            return False
        # The wildcard takes one more character, and the units after it are tried again.
        after_wildcard += 1
        at_text = after_wildcard
        at_unit = wildcard + 1
    while at_unit < held and units[at_unit][0] == "any":
        at_unit += 1
    return at_unit == held


def _like(held, pattern):
    """`LIKE`: whether the expression's text matches the pattern the other names.

    The pattern is read into its units ONCE per value it takes rather than once per row,
    because the pattern of a `LIKE` is the same for every row of a dataset.
    """
    read_patterns = {}

    def read(row):
        left = held.of(row)
        spelled = pattern.of(row)
        if left is None or spelled is None:
            return None
        spelled = str(spelled)
        units = read_patterns.get(spelled)
        if units is None:
            if len(read_patterns) >= MAX_LIKE_PATTERNS:
                read_patterns.clear()
            units = _like_pattern(spelled)
            read_patterns[spelled] = units
        return _like_matches(str(left), units)

    return Column("(%s LIKE %s)" % (held._name, pattern._name), read)


class _Reader(object):
    """One expression's tokens, and the descent over the grammar that reads them."""

    def __init__(self, text):
        if len(text) > MAX_EXPRESSION_CHARACTERS:
            raise AnalysisException("the expression is too long to read")
        self._text = text
        self._tokens = _tokens(text)
        self._at = 0
        self._depth = 0

    # --- the tokens --------------------------------------------------------------------

    def _peek(self, ahead=0):
        at = self._at + ahead
        if at < len(self._tokens):
            return self._tokens[at]
        return ("end", "")

    def _take(self):
        token = self._peek()
        self._at += 1
        return token

    def _word(self, ahead=0):
        """The next token as the lower-case word it is, or None where it is no name."""
        kind, text = self._peek(ahead)
        return str(text).lower() if kind == "name" else None

    def _keyword(self, *words):
        """Take the next token when it is one of these keywords, answering which."""
        held = self._word()
        if held is not None and held in words:
            self._at += 1
            return held
        return None

    def _operator(self, *spelled):
        kind, text = self._peek()
        if kind == "operator" and text in spelled:
            self._at += 1
            return text
        return None

    def _expect(self, spelled):
        if self._operator(spelled) is None:
            raise AnalysisException(
                "cannot read the expression %s: expected %s" % (self._text, spelled)
            )

    def at_end(self):
        """Refuse an expression with tokens left over, rather than reading part of one."""
        if self._peek()[0] != "end":
            raise AnalysisException(
                "cannot read the expression %s: %s is left over"
                % (self._text, self._peek()[1])
            )

    # --- the grammar -------------------------------------------------------------------

    def expression(self):
        """One whole expression, at the outermost precedence."""
        self._deeper()
        held = self._disjunction()
        self._depth -= 1
        return held

    def _deeper(self):
        """Take one level of the nesting an expression may reach, or refuse it.

        Every production that recurses on the expression's own text takes one, so a chain of
        prefix operators (`not not …`, `- - …`) is answered by this bound rather than by the
        interpreter's own recursion limit.
        """
        self._depth += 1
        if self._depth > MAX_EXPRESSION_DEPTH:
            raise AnalysisException("the expression %s nests too deeply" % (self._text,))

    def _disjunction(self):
        held = self._conjunction()
        while self._keyword("or"):
            held = held | self._conjunction()
        return held

    def _conjunction(self):
        held = self._negation()
        while self._keyword("and"):
            held = held & self._negation()
        return held

    def _negation(self):
        if self._keyword("not"):
            self._deeper()
            held = ~self._negation()
            self._depth -= 1
            return held
        return self._comparison()

    def _comparison(self):
        held = self._sum()
        while True:
            spelled = self._operator("=", "==", "<>", "!=", "<", "<=", ">", ">=")
            if spelled is not None:
                held = _compared(held, spelled, self._sum())
                continue
            if self._keyword("is"):
                negated = self._keyword("not") is not None
                if self._keyword("null") is None:
                    raise AnalysisException(
                        "cannot read the expression %s: IS names what it tests for"
                        % (self._text,)
                    )
                held = held.isNotNull() if negated else held.isNull()
                continue
            # A `NOT` here belongs to the test after it (`NOT IN`, `NOT BETWEEN`,
            # `NOT LIKE`); one standing before anything else is the negation the
            # precedence above reads, so the reading goes back to where it was.
            at = self._at
            negated = self._keyword("not") is not None
            word = self._keyword("in", "between", "like")
            if word is None:
                self._at = at
                return held
            if word == "in":
                self._expect("(")
                values = [self.expression()]
                while self._operator(","):
                    values.append(self.expression())
                self._expect(")")
                held = _within(held, values)
            elif word == "between":
                lower = self._sum()
                if self._keyword("and") is None:
                    raise AnalysisException(
                        "BETWEEN names both bounds: %s" % (self._text,)
                    )
                held = held.between(lower, self._sum())
            else:
                held = _like(held, self._sum())
            if negated:
                held = ~held
        return held

    def _sum(self):
        held = self._product()
        while True:
            spelled = self._operator("+", "-", "||")
            if spelled is None:
                return held
            right = self._product()
            if spelled == "+":
                held = held + right
            elif spelled == "-":
                held = held - right
            else:
                # `||` runs the text of its operands together, which is `concat`.
                held = functions.concat(held, right)

    def _product(self):
        held = self._unary()
        while True:
            spelled = self._operator("*", "/", "%")
            if spelled is None:
                return held
            right = self._unary()
            if spelled == "*":
                held = held * right
            elif spelled == "/":
                held = held / right
            else:
                held = held % right

    def _unary(self):
        if self._operator("-") is not None:
            self._deeper()
            held = -self._unary()
            self._depth -= 1
            return held
        if self._operator("+") is not None:
            self._deeper()
            held = self._unary()
            self._depth -= 1
            return held
        return self._primary()

    def _primary(self):
        kind, text = self._peek()
        if kind == "operator" and text == "(":
            self._at += 1
            held = self.expression()
            self._expect(")")
            return held
        if kind in ("number", "string"):
            self._at += 1
            return literal(text)
        if kind == "name":
            word = str(text).lower()
            if word == "null":
                self._at += 1
                return literal(None)
            if word in ("true", "false"):
                self._at += 1
                return literal(word == "true")
            if word == "case":
                return self._case()
            if word == "cast":
                return self._cast()
            if self._peek(1) == ("operator", "("):
                return self._called()
            if word in KEYWORDS:
                raise AnalysisException(
                    "cannot read the expression %s: %s is a keyword of the grammar"
                    % (self._text, text)
                )
            self._at += 1
            return field(text)
        raise AnalysisException(
            "cannot read the expression %s: %s names no value" % (self._text, text)
        )

    def _cast(self):
        """`cast(<expression> as <type>)`: the same values read as the type named."""
        self._at += 1
        self._expect("(")
        held = self.expression()
        if self._keyword("as") is None:
            raise AnalysisException(
                "a cast names the type to read the value as: %s" % (self._text,)
            )
        kind = self._type()
        self._expect(")")
        return held.cast(kind)

    def _type(self):
        """The data type a cast names, as it is spelled."""
        kind, text = self._take()
        if kind != "name":
            raise AnalysisException("cannot resolve the data type %s" % (text,))
        spelled = str(text)
        if self._operator("(") is not None:
            # A parameterised type (`decimal(10, 2)`) is read whole, so what cannot be
            # resolved is named as the program wrote it.
            held = [spelled, "("]
            while self._operator(")") is None:
                piece = self._take()
                if piece[0] == "end":
                    raise AnalysisException(
                        "cannot resolve the data type %s" % (spelled,)
                    )
                held.append(str(piece[1]))
            spelled = "".join(held) + ")"
        return spelled

    def _case(self):
        """`CASE [<operand>] WHEN … THEN … [ELSE …] END`, as the conditional it is."""
        self._at += 1
        operand = None
        if self._word() != "when":
            operand = self.expression()
        branches = []
        while self._keyword("when"):
            condition = self.expression()
            if self._keyword("then") is None:
                raise AnalysisException(
                    "a CASE branch names what it answers: %s" % (self._text,)
                )
            value = self.expression()
            if operand is not None:
                condition = operand == condition
            branches.append((condition, value))
        default = None
        if self._keyword("else"):
            default = self.expression()
        if self._keyword("end") is None:
            raise AnalysisException(
                "a CASE expression ends with END: %s" % (self._text,)
            )
        if not branches:
            raise AnalysisException(
                "a CASE expression names at least one WHEN: %s" % (self._text,)
            )
        return When(branches, default)

    def _called(self):
        """One function call, as the expression the catalog's function answers."""
        name = str(self._take()[1])
        lowered = name.lower()
        self._expect("(")
        distinct = self._keyword("distinct") is not None
        if self._peek() == ("operator", "*") and self._peek(1) == ("operator", ")"):
            # `count(*)` counts the ROWS of the group rather than a column's values.
            self._at += 2
            if lowered != "count":
                raise AnalysisException("Undefined function: %s" % (name,))
            return aggregate("count", None)
        arguments = []
        if self._operator(")") is None:
            arguments.append(self.expression())
            while self._operator(","):
                arguments.append(self.expression())
            self._expect(")")
        if lowered in AGGREGATES:
            if len(arguments) != 1:
                raise AnalysisException(
                    "%s takes one expression, not %d" % (lowered, len(arguments))
                )
            return aggregate(lowered, arguments[0], distinct)
        if lowered not in CATALOG:
            raise AnalysisException("Undefined function: %s" % (name,))
        return getattr(functions, CATALOG[lowered])(*arguments)

    def alias(self):
        """The name a trailing `AS <name>` — or a bare name — gives the expression."""
        if self._keyword("as") is not None:
            kind, text = self._take()
            if kind not in ("name", "string"):
                raise AnalysisException(
                    "an alias names the column: %s" % (self._text,)
                )
            return str(text)
        kind, text = self._peek()
        if kind == "name" and str(text).lower() not in KEYWORDS:
            self._at += 1
            return str(text)
        return None


def read_sql_expression(text):
    """One SQL expression, as the COLUMN EXPRESSION it names.

    The grammar is the one a projection's expressions are written in, so a form outside it
    raises that grammar's own analysis failure rather than answering an expression nothing
    computes. An expression that names no alias of its own carries the text it was written
    as, which is the name a projection of it then holds.
    """
    spelled = str(text)
    try:
        reader = _Reader(spelled)
        held = reader.expression()
        named = reader.alias()
        reader.at_end()
    except RecursionError:
        # The reading recurses on the expression's own text, and one level of nesting costs
        # several frames — so an expression deep enough to reach the INTERPRETER's own
        # recursion limit before this reading's nesting bound is still an expression that
        # nests deeper than the reading goes, and is answered by that bound rather than by an
        # error of the interpreter leaking out of a projection.
        raise AnalysisException("the expression %s nests too deeply" % (spelled,))
    return held.alias(named if named is not None else spelled.strip())


__all__ = ["read_sql_expression"]
"#;
