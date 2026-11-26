// SPDX-License-Identifier: CC0-1.0

//! `jmespath`: the QUERY LANGUAGE the AWS SDK for Python is installed with, which a managed
//! `python3.x` runtime therefore carries on a handler's import path.
//!
//! The SDK's own data-driven interfaces are written over it: a paginator's continuation,
//! page-size and result members are JMESPath expressions in the service's pagination data, a
//! waiter's acceptors match with them, and a resource's identifiers and references are read
//! with them. So the SDK's installation requires this distribution, a managed runtime
//! preinstalls it, and a deployment package that bundles the SDK itself but not its
//! dependencies still imports: the package's copy of `boto3` and `botocore` wins over the
//! runtime's, while `import jmespath` resolves from the runtime behind it.
//!
//! What these modules do and do not do:
//!
//! * The LANGUAGE is the published one: an expression is TOKENIZED, PARSED into the syntax
//!   tree the library's own node types describe, and INTERPRETED against the data — no
//!   expression is matched as text and nothing is special-cased for one caller. Identifiers
//!   (quoted and unquoted), sub-expressions, indices and slices, the three projections and the
//!   flatten, filters, multi-selects, pipes, the boolean and comparison operators, literals
//!   and raw strings, function calls and expression references are all evaluated as the
//!   specification states them, including its own rules for what is truthy and which types a
//!   comparison admits.
//! * The SURFACE is the library's: `jmespath.compile` answers a parsed expression that can be
//!   searched more than once, `jmespath.search` is the one-shot form, `jmespath.Options`
//!   carries the dictionary class a multi-select builds and a caller's own functions, and the
//!   errors are the library's own types with its own wording — so the SDK and a handler
//!   written against the published library run unchanged.
//! * A caller's own function class (`jmespath.functions.Functions`) is what a function call is
//!   dispatched through, so the functions the specification defines and the ones a caller adds
//!   are reached the same way.

/// The modules of the query language, under the paths the package holds them at.
#[must_use]
pub(super) fn modules() -> Vec<(&'static str, &'static str)> {
    vec![
        ("jmespath/__init__.py", INIT_SOURCE),
        ("jmespath/exceptions.py", EXCEPTIONS_SOURCE),
        ("jmespath/compat.py", COMPAT_SOURCE),
        ("jmespath/ast.py", AST_SOURCE),
        ("jmespath/lexer.py", LEXER_SOURCE),
        ("jmespath/parser.py", PARSER_SOURCE),
        ("jmespath/functions.py", FUNCTIONS_SOURCE),
        ("jmespath/visitor.py", VISITOR_SOURCE),
    ]
}

/// `jmespath`: the two calls a caller makes, and the options they take.
const INIT_SOURCE: &str = r#"
"""The JMESPath query language, as this runtime supplies it."""

from jmespath import parser
from jmespath.visitor import Options

# The GENERATION of the library these modules implement, which is the one a managed
# `python3.x` runtime ships beside its SDK. It is not a published release of the library: no
# release of one is here to name.
__version__ = "1.0.0"

__all__ = ["compile", "search", "Options"]


def compile(expression):
    """The expression `expression` states, parsed once and searchable many times."""
    return parser.Parser().parse(expression)


def search(expression, data, options=None):
    """What `expression` reaches in `data`."""
    return parser.Parser().parse(expression).search(data, options=options)
"#;

/// `jmespath.exceptions`: the errors reading or evaluating an expression raises.
const EXCEPTIONS_SOURCE: &str = r#"
"""The errors an expression raises."""


class JMESPathError(ValueError):
    """Every failure of this library is one of these."""


class ParseError(JMESPathError):
    """The expression is not one the language admits."""

    _ERROR_MESSAGE = "Invalid jmespath expression"

    def __init__(self, lex_position, token_value, token_type, msg=_ERROR_MESSAGE):
        JMESPathError.__init__(self)
        self.lex_position = lex_position
        self.token_value = token_value
        self.token_type = str(token_type)
        self.msg = msg
        self.expression = None

    def __str__(self):
        underline = " " * (self.lex_position + 1) + "^"
        return "%s: Parse error at column %s, token \"%s\" (%s), for expression:\n\"%s\"\n%s" % (
            self.msg,
            self.lex_position,
            self.token_value,
            self.token_type,
            self.expression,
            underline,
        )


class IncompleteExpressionError(ParseError):
    """The expression ended in the middle of something."""

    def set_expression(self, expression):
        self.expression = expression
        self.lex_position = len(expression)
        self.token_type = None
        self.token_value = None

    def __str__(self):
        underline = " " * (self.lex_position + 1) + "^"
        return "Invalid jmespath expression: Incomplete expression:\n\"%s\"\n%s" % (
            self.expression,
            underline,
        )


class LexerError(ParseError):
    """A character of the expression is not one the language has."""

    def __init__(self, lexer_position, lexer_value, message, expression=None):
        self.lexer_position = lexer_position
        self.lexer_value = lexer_value
        self.message = message
        ParseError.__init__(
            self,
            lex_position=lexer_position,
            token_value=lexer_value,
            token_type="unknown",
            msg=message,
        )
        self.expression = expression

    def __str__(self):
        underline = " " * (self.lexer_position + 1) + "^"
        return "Bad jmespath expression: %s:\n\"%s\"\n%s" % (
            self.message,
            self.expression,
            underline,
        )


class ArityError(ParseError):
    """A function was called with the wrong number of arguments."""

    def __init__(self, expected, actual, name):
        self.expected_arity = expected
        self.actual_arity = actual
        self.function_name = name
        self.expression = None

    def __str__(self):
        return "Expected %s %s for function %s(), received %s" % (
            self.expected_arity,
            self._pluralize("argument", self.expected_arity),
            self.function_name,
            self.actual_arity,
        )

    def _pluralize(self, word, count):
        if count == 1:
            return word
        return word + "s"


class VariadictArityError(ArityError):
    """A function taking at least so many arguments was called with fewer."""

    def __str__(self):
        return "Expected at least %s %s for function %s(), received %s" % (
            self.expected_arity,
            self._pluralize("argument", self.expected_arity),
            self.function_name,
            self.actual_arity,
        )


class JMESPathTypeError(JMESPathError):
    """A function was given a value of a type it does not take."""

    def __init__(self, function_name, current_value, actual_type, expected_types):
        self.function_name = function_name
        self.current_value = current_value
        self.actual_type = actual_type
        self.expected_types = expected_types

    def __str__(self):
        return (
            "In function %s(), invalid type for value: %s, "
            "expected one of: %s, received: \"%s\""
            % (
                self.function_name,
                self.current_value,
                self.expected_types,
                self.actual_type,
            )
        )


class EmptyExpressionError(JMESPathError):
    """Nothing was given to parse."""

    def __init__(self):
        JMESPathError.__init__(self, "Invalid JMESPath expression: cannot be empty.")


class UnknownFunctionError(JMESPathError):
    """A function the language does not define, and no caller added, was called."""
"#;

/// `jmespath.compat`: what the library reads of the interpreter it runs on.
const COMPAT_SOURCE: &str = r#"
"""What this library reads of the interpreter it runs on."""

import inspect


def with_str_method(cls):
    """The class, with its own text rendering, which this interpreter already uses."""
    return cls


def with_repr_method(cls):
    return cls


def get_methods(cls):
    """Every method of `cls`, which is how a function table is read off a class."""
    for name, method in inspect.getmembers(cls, predicate=inspect.isfunction):
        yield name, method


string_type = str
text_type = str
"#;

/// `jmespath.ast`: the NODES a parsed expression is built of.
const AST_SOURCE: &str = r#"
"""The nodes a parsed expression is built of.

Each one is the document the library's own parser builds and its interpreter visits: a type,
the child nodes under it and, where the node carries one, a value of its own.
"""


def comparator(name, first, second):
    return {"type": "comparator", "children": [first, second], "value": name}


def current_node():
    return {"type": "current", "children": []}


def expref(expression):
    return {"type": "expref", "children": [expression]}


def function_expression(name, args):
    return {"type": "function_expression", "children": args, "value": name}


def field(name):
    return {"type": "field", "children": [], "value": name}


def filter_projection(left, right, comparator):
    return {"type": "filter_projection", "children": [left, right, comparator]}


def flatten(node):
    return {"type": "flatten", "children": [node]}


def identity():
    return {"type": "identity", "children": []}


def index(index):
    return {"type": "index", "value": index, "children": []}


def index_expression(children):
    return {"type": "index_expression", "children": children}


def key_val_pair(key_name, node):
    return {"type": "key_val_pair", "children": [node], "value": key_name}


def literal(literal_value):
    return {"type": "literal", "value": literal_value, "children": []}


def multi_select_dict(nodes):
    return {"type": "multi_select_dict", "children": nodes}


def multi_select_list(nodes):
    return {"type": "multi_select_list", "children": nodes}


def or_expression(left, right):
    return {"type": "or_expression", "children": [left, right]}


def and_expression(left, right):
    return {"type": "and_expression", "children": [left, right]}


def not_expression(expr):
    return {"type": "not_expression", "children": [expr]}


def pipe(left, right):
    return {"type": "pipe", "children": [left, right]}


def projection(left, right):
    return {"type": "projection", "children": [left, right]}


def subexpression(children):
    return {"type": "subexpression", "children": children}


def slice(start, end, step):
    return {"type": "slice", "children": [start, end, step]}


def value_projection(left, right):
    return {"type": "value_projection", "children": [left, right]}
"#;

/// `jmespath.lexer`: the TOKENS an expression is read as.
const LEXER_SOURCE: &str = r#"
"""The tokens an expression is read as."""

import json
import string

from jmespath.exceptions import EmptyExpressionError, LexerError

# The characters a name may start with and continue with, which is what an unquoted
# identifier is.
START_IDENTIFIER = set(string.ascii_letters + "_")
VALID_IDENTIFIER = set(string.ascii_letters + string.digits + "_")
VALID_NUMBER = set(string.digits)
WHITESPACE = set(" \t\n\r")

# The one-character tokens, each standing for itself.
SIMPLE_TOKENS = {
    ".": "dot",
    "*": "star",
    "]": "rbracket",
    ",": "comma",
    ":": "colon",
    "@": "current",
    "(": "lparen",
    ")": "rparen",
    "{": "lbrace",
    "}": "rbrace",
}


class Lexer:
    """Reads one expression into the tokens it is made of."""

    def tokenize(self, expression):
        self._initialize_for_expression(expression)
        while self._current is not None:
            if self._current in SIMPLE_TOKENS:
                yield {
                    "type": SIMPLE_TOKENS[self._current],
                    "value": self._current,
                    "start": self._position,
                    "end": self._position + 1,
                }
                self._next()
            elif self._current in START_IDENTIFIER:
                start = self._position
                buff = self._current
                while self._next() in VALID_IDENTIFIER:
                    buff += self._current
                yield {
                    "type": "unquoted_identifier",
                    "value": buff,
                    "start": start,
                    "end": start + len(buff),
                }
            elif self._current in WHITESPACE:
                self._next()
            elif self._current == "[":
                start = self._position
                next_char = self._next()
                if next_char == "]":
                    self._next()
                    yield {"type": "flatten", "value": "[]", "start": start, "end": start + 2}
                elif next_char == "?":
                    self._next()
                    yield {"type": "filter", "value": "[?", "start": start, "end": start + 2}
                else:
                    yield {"type": "lbracket", "value": "[", "start": start, "end": start + 1}
            elif self._current == "'":
                yield self._consume_raw_string_literal()
            elif self._current == "|":
                yield self._match_or_else("|", "or", "pipe")
            elif self._current == "&":
                yield self._match_or_else("&", "and", "expref")
            elif self._current == "`":
                yield self._consume_literal()
            elif self._current in VALID_NUMBER:
                start = self._position
                buff = self._consume_number()
                yield {
                    "type": "number",
                    "value": int(buff),
                    "start": start,
                    "end": start + len(buff),
                }
            elif self._current == "-":
                yield self._consume_negative_number()
            elif self._current == '"':
                yield self._consume_quoted_identifier()
            elif self._current == "<":
                yield self._match_or_else("=", "lte", "lt")
            elif self._current == ">":
                yield self._match_or_else("=", "gte", "gt")
            elif self._current == "!":
                yield self._match_or_else("=", "ne", "not")
            elif self._current == "=":
                if self._next() == "=":
                    self._next()
                    yield {
                        "type": "eq",
                        "value": "==",
                        "start": self._position - 1,
                        "end": self._position,
                    }
                else:
                    if self._current is None:
                        raise LexerError(
                            lexer_position=self._position,
                            lexer_value="=",
                            message="Unknown token '='",
                        )
                    raise LexerError(
                        lexer_position=self._position - 1,
                        lexer_value="=",
                        message="Unknown token '=%s'" % self._current,
                    )
            else:
                raise LexerError(
                    lexer_position=self._position,
                    lexer_value=self._current,
                    message="Unknown token %s" % self._current,
                )
        yield {"type": "eof", "value": "", "start": self._length, "end": self._length}

    def _consume_number(self):
        start = self._position
        buff = self._current
        while self._next() in VALID_NUMBER:
            buff += self._current
        return buff

    def _initialize_for_expression(self, expression):
        if not expression:
            raise EmptyExpressionError()
        self._position = 0
        self._expression = expression
        self._chars = list(self._expression)
        self._current = self._chars[self._position]
        self._length = len(self._expression)

    def _next(self):
        if self._position == self._length - 1:
            self._current = None
        else:
            self._position += 1
            self._current = self._chars[self._position]
        return self._current

    def _consume_until(self, delimiter):
        """Everything up to the next unescaped `delimiter`, which is where a quoted run
        ends."""
        start = self._position
        buff = ""
        self._next()
        while self._current != delimiter:
            if self._current == "\\":
                buff += "\\"
                self._next()
            if self._current is None:
                raise LexerError(
                    lexer_position=start,
                    lexer_value=self._expression[start:],
                    message="Unclosed %s delimiter" % delimiter,
                )
            buff += self._current
            self._next()
        self._next()
        return buff

    def _consume_literal(self):
        start = self._position
        lexeme = self._consume_until("`").replace("\\`", "`")
        try:
            parsed_json = json.loads(lexeme)
        except ValueError:
            # A literal the language admits that is not JSON is the string it reads as,
            # which is what the published library falls back to.
            try:
                parsed_json = json.loads('"%s"' % lexeme.lstrip())
            except ValueError:
                raise LexerError(
                    lexer_position=start,
                    lexer_value=self._expression[start:],
                    message="Bad token %s" % lexeme,
                )
        token_len = self._position - start
        return {
            "type": "literal",
            "value": parsed_json,
            "start": start,
            "end": token_len,
        }

    def _consume_quoted_identifier(self):
        start = self._position
        lexeme = '"' + self._consume_until('"') + '"'
        try:
            token_len = self._position - start
            return {
                "type": "quoted_identifier",
                "value": json.loads(lexeme),
                "start": start,
                "end": token_len,
            }
        except ValueError as error:
            raise LexerError(
                lexer_position=start,
                lexer_value=lexeme,
                message="Bad token %s (%s)" % (lexeme, error),
            )

    def _consume_raw_string_literal(self):
        start = self._position
        lexeme = self._consume_until("'").replace("\\'", "'")
        token_len = self._position - start
        return {
            "type": "literal",
            "value": lexeme,
            "start": start,
            "end": token_len,
        }

    def _consume_negative_number(self):
        start = self._position
        buff = "-"
        while self._next() in VALID_NUMBER:
            buff += self._current
        if len(buff) > 1:
            return {
                "type": "number",
                "value": int(buff),
                "start": start,
                "end": start + len(buff),
            }
        raise LexerError(
            lexer_position=start,
            lexer_value=buff,
            message="Unknown token '%s'" % buff,
        )

    def _match_or_else(self, expected, match_type, else_type):
        start = self._position
        current = self._current
        next_char = self._next()
        if next_char == expected:
            self._next()
            return {
                "type": match_type,
                "value": current + next_char,
                "start": start,
                "end": start + 1,
            }
        return {
            "type": else_type,
            "value": current,
            "start": start,
            "end": start,
        }
"#;

/// `jmespath.parser`: the TREE an expression is parsed into, and the result it is searched
/// through.
const PARSER_SOURCE: &str = r#"
"""The tree an expression is parsed into.

The parse is the published library's own: each token has a BINDING POWER, a token that can
start an expression has a rule for that position and a token that can follow one has a rule
for that, so an expression is read by reading a leading token and then consuming every
following token that binds tighter than the caller asked for.
"""

import random

from jmespath import ast, exceptions, lexer
from jmespath.visitor import GraphvizVisitor, Options, TreeInterpreter


class Parser:
    """Reads an expression into the tree it states."""

    BINDING_POWER = {
        "eof": 0,
        "unquoted_identifier": 0,
        "quoted_identifier": 0,
        "literal": 0,
        "rbracket": 0,
        "rparen": 0,
        "comma": 0,
        "rbrace": 0,
        "number": 0,
        "current": 0,
        "expref": 0,
        "colon": 0,
        "pipe": 1,
        "or": 2,
        "and": 3,
        "eq": 5,
        "gt": 5,
        "lt": 5,
        "gte": 5,
        "lte": 5,
        "ne": 5,
        "flatten": 9,
        "star": 20,
        "filter": 21,
        "dot": 40,
        "not": 45,
        "lbrace": 50,
        "lbracket": 55,
        "lparen": 60,
    }

    # The tokens a PROJECTION stops at: everything that binds looser than the projection
    # itself, which is what makes a pipe end one.
    _PROJECTION_STOP = 10

    # How many parsed expressions are kept, so an expression parsed again is not read again.
    _MAX_SIZE = 128

    _CACHE = {}

    def __init__(self, lookahead=2):
        # How many tokens past the current one a rule may read. It is NOT held under the name
        # of the reader below (`_lookahead`), which would shadow it.
        self._buffer_size = lookahead
        self._tokens = [None] * lookahead
        self._index = 0

    def parse(self, expression):
        cached = self._CACHE.get(expression)
        if cached is not None:
            return cached
        parsed_result = self._do_parse(expression)
        self._CACHE[expression] = parsed_result
        if len(self._CACHE) > self._MAX_SIZE:
            self._free_cache_entries()
        return parsed_result

    def _do_parse(self, expression):
        try:
            return self._parse(expression)
        except exceptions.LexerError as error:
            error.expression = expression
            raise
        except exceptions.IncompleteExpressionError as error:
            error.set_expression(expression)
            raise
        except exceptions.ParseError as error:
            error.expression = expression
            raise

    def _parse(self, expression):
        self._expression = expression
        self._index = 0
        self._tokens = list(lexer.Lexer().tokenize(expression))
        parsed = self._expression_at(binding_power=0)
        if not self._current_token() == "eof":
            token = self._lookahead_token(0)
            raise exceptions.ParseError(
                token["start"], token["value"], token["type"], "Unexpected token: %s" % token["value"]
            )
        return ParsedResult(expression, parsed)

    def _expression_at(self, binding_power):
        left_token = self._lookahead_token(0)
        self._advance()
        nud_function = getattr(
            self, "_token_nud_%s" % left_token["type"], self._error_nud_token
        )
        left = nud_function(left_token)
        current_token = self._current_token()
        while binding_power < self.BINDING_POWER[current_token]:
            led = getattr(self, "_token_led_%s" % current_token, None)
            if led is None:
                self._error_led_token(self._lookahead_token(0))
            else:
                self._advance()
                left = led(left)
                current_token = self._current_token()
        return left

    def _token_nud_literal(self, token):
        return ast.literal(token["value"])

    def _token_nud_unquoted_identifier(self, token):
        return ast.field(token["value"])

    def _token_nud_quoted_identifier(self, token):
        field = ast.field(token["value"])
        # A quoted identifier is never a function name: `"foo"(` is not a call.
        if self._current_token() == "lparen":
            token = self._lookahead_token(0)
            raise exceptions.ParseError(
                0, token["value"], token["type"], "Quoted identifier not allowed for function names."
            )
        return field

    def _token_nud_star(self, token):
        left = ast.identity()
        if self._current_token() == "rbracket":
            right = ast.identity()
        else:
            right = self._parse_projection_rhs(self.BINDING_POWER["star"])
        return ast.value_projection(left, right)

    def _token_nud_filter(self, token):
        return self._token_led_filter(ast.identity())

    def _token_nud_lbrace(self, token):
        return self._parse_multi_select_hash()

    def _token_nud_flatten(self, token):
        left = ast.flatten(ast.identity())
        right = self._parse_projection_rhs(self.BINDING_POWER["flatten"])
        return ast.projection(left, right)

    def _token_nud_not(self, token):
        expr = self._expression_at(self.BINDING_POWER["not"])
        return ast.not_expression(expr)

    def _token_nud_lparen(self, token):
        expression = self._expression_at(0)
        self._match("rparen")
        return expression

    def _token_nud_lbracket(self, token):
        if self._current_token() in ["number", "colon"]:
            right = self._parse_index_expression()
            # A bare index at the start of an expression is read against the data itself.
            return self._project_if_slice(ast.identity(), right)
        if self._current_token() == "star" and self._lookahead(1) == "rbracket":
            self._advance()
            self._advance()
            right = self._parse_projection_rhs(self.BINDING_POWER["star"])
            return ast.projection(ast.identity(), right)
        return self._parse_multi_select_list()

    def _token_nud_current(self, token):
        return ast.current_node()

    def _token_nud_expref(self, token):
        expression = self._expression_at(self.BINDING_POWER["expref"])
        return ast.expref(expression)

    def _token_led_dot(self, left):
        if not self._current_token() == "star":
            right = self._parse_dot_rhs(self.BINDING_POWER["dot"])
            if left["type"] == "subexpression":
                left["children"].append(right)
                return left
            return ast.subexpression([left, right])
        # A `.*` is a projection over the values of what is on the left.
        self._advance()
        right = self._parse_projection_rhs(self.BINDING_POWER["dot"])
        return ast.value_projection(left, right)

    def _token_led_pipe(self, left):
        right = self._expression_at(self.BINDING_POWER["pipe"])
        return ast.pipe(left, right)

    def _token_led_or(self, left):
        right = self._expression_at(self.BINDING_POWER["or"])
        return ast.or_expression(left, right)

    def _token_led_and(self, left):
        right = self._expression_at(self.BINDING_POWER["and"])
        return ast.and_expression(left, right)

    def _token_led_lparen(self, left):
        if left["type"] != "field":
            #  0 - first func arg or closing paren.
            # -1 - '(' token
            # -2 - invalid function "name".
            prev_t = self._lookahead_token(-2)
            raise exceptions.ParseError(
                prev_t["start"],
                prev_t["value"],
                prev_t["type"],
                "Invalid function name '%s'" % prev_t["value"],
            )
        name = left["value"]
        args = []
        while not self._current_token() == "rparen":
            expression = self._expression_at(0)
            if self._current_token() == "comma":
                self._match("comma")
            args.append(expression)
        self._match("rparen")
        return ast.function_expression(name, args)

    def _token_led_filter(self, left):
        condition = self._expression_at(0)
        self._match("rbracket")
        if self._current_token() == "flatten":
            right = ast.identity()
        else:
            right = self._parse_projection_rhs(self.BINDING_POWER["filter"])
        return ast.filter_projection(left, right, condition)

    def _token_led_eq(self, left):
        return self._parse_comparator(left, "eq")

    def _token_led_ne(self, left):
        return self._parse_comparator(left, "ne")

    def _token_led_gt(self, left):
        return self._parse_comparator(left, "gt")

    def _token_led_gte(self, left):
        return self._parse_comparator(left, "gte")

    def _token_led_lt(self, left):
        return self._parse_comparator(left, "lt")

    def _token_led_lte(self, left):
        return self._parse_comparator(left, "lte")

    def _token_led_flatten(self, left):
        left = ast.flatten(left)
        right = self._parse_projection_rhs(self.BINDING_POWER["flatten"])
        return ast.projection(left, right)

    def _token_led_lbracket(self, left):
        token = self._lookahead_token(0)
        if token["type"] in ["number", "colon"]:
            right = self._parse_index_expression()
            if left["type"] == "index_expression":
                left["children"].append(right)
                return left
            return self._project_if_slice(left, right)
        # A `[*]` is a projection over the elements of what is on the left.
        self._match("star")
        self._match("rbracket")
        right = self._parse_projection_rhs(self.BINDING_POWER["star"])
        return ast.projection(left, right)

    def _project_if_slice(self, left, right):
        index_expr = ast.index_expression([left, right])
        if right["type"] == "slice":
            return ast.projection(
                index_expr, self._parse_projection_rhs(self.BINDING_POWER["star"])
            )
        return index_expr

    def _parse_index_expression(self):
        # A slice is `start:end:step`, each of them optional.
        if self._lookahead(0) == "colon" or self._lookahead(1) == "colon":
            return self._parse_slice_expression()
        node = ast.index(self._lookahead_token(0)["value"])
        self._advance()
        self._match("rbracket")
        return node

    def _parse_slice_expression(self):
        parts = [None, None, None]
        index = 0
        current_token = self._current_token()
        while not current_token == "rbracket" and index < 3:
            if current_token == "colon":
                index += 1
                if index == 3:
                    self._raise_parse_error_for_token(
                        self._lookahead_token(0), "syntax error"
                    )
                self._advance()
            elif current_token == "number":
                parts[index] = self._lookahead_token(0)["value"]
                self._advance()
            else:
                self._raise_parse_error_for_token(
                    self._lookahead_token(0), "syntax error"
                )
            current_token = self._current_token()
        self._match("rbracket")
        return ast.slice(*parts)

    def _parse_comparator(self, left, comparator):
        right = self._expression_at(self.BINDING_POWER[comparator])
        return ast.comparator(comparator, left, right)

    def _parse_multi_select_list(self):
        expressions = []
        while True:
            expression = self._expression_at(0)
            expressions.append(expression)
            if self._current_token() == "rbracket":
                break
            self._match("comma")
        self._match("rbracket")
        return ast.multi_select_list(expressions)

    def _parse_multi_select_hash(self):
        pairs = []
        while True:
            key_token = self._lookahead_token(0)
            if key_token["type"] not in ["quoted_identifier", "unquoted_identifier"]:
                self._raise_parse_error_for_token(
                    key_token, "expecting an identifier"
                )
            key_name = key_token["value"]
            self._advance()
            self._match("colon")
            value = self._expression_at(0)
            node = ast.key_val_pair(key_name=key_name, node=value)
            pairs.append(node)
            if self._current_token() == "comma":
                self._match("comma")
            elif self._current_token() == "rbrace":
                self._match("rbrace")
                break
        return ast.multi_select_dict(nodes=pairs)

    def _parse_projection_rhs(self, binding_power):
        """What a projection applies to each element it projects over."""
        if self.BINDING_POWER[self._current_token()] < self._PROJECTION_STOP:
            # A projection with nothing after it projects each element as it is.
            right = ast.identity()
        elif self._current_token() == "lbracket":
            right = self._expression_at(binding_power)
        elif self._current_token() == "filter":
            right = self._expression_at(binding_power)
        elif self._current_token() == "dot":
            self._match("dot")
            right = self._parse_dot_rhs(binding_power)
        else:
            self._raise_parse_error_for_token(self._lookahead_token(0), "syntax error")
        return right

    def _parse_dot_rhs(self, binding_power):
        # A dot can be followed by a name, a multi-select of either kind, a star or a
        # bracket specifier.
        lookahead = self._current_token()
        if lookahead in ["quoted_identifier", "unquoted_identifier", "star"]:
            return self._expression_at(binding_power)
        if lookahead == "lbracket":
            self._match("lbracket")
            return self._parse_multi_select_list()
        if lookahead == "lbrace":
            self._match("lbrace")
            return self._parse_multi_select_hash()
        self._raise_parse_error_for_token(
            self._lookahead_token(0),
            "syntax error",
        )

    def _error_nud_token(self, token):
        if token["type"] == "eof":
            raise exceptions.IncompleteExpressionError(
                token["start"], token["value"], token["type"]
            )
        self._raise_parse_error_for_token(token, "invalid token")

    def _error_led_token(self, token):
        self._raise_parse_error_for_token(token, "invalid token")

    def _match(self, token_type=None):
        # The next token has to be `token_type`; a token that is not is the parse error that
        # names it.
        if self._current_token() == token_type:
            self._advance()
        else:
            self._raise_parse_error_maybe_eof(token_type, self._lookahead_token(0))

    def _raise_parse_error_for_token(self, token, reason):
        lex_position = token["start"]
        actual_value = token["value"]
        actual_type = token["type"]
        raise exceptions.ParseError(lex_position, actual_value, actual_type, reason)

    def _raise_parse_error_maybe_eof(self, expected_type, token):
        lex_position = token["start"]
        actual_value = token["value"]
        actual_type = token["type"]
        if actual_type == "eof":
            raise exceptions.IncompleteExpressionError(
                lex_position, actual_value, actual_type
            )
        message = "Expecting: %s, got: %s" % (expected_type, actual_type)
        raise exceptions.ParseError(lex_position, actual_value, actual_type, message)

    def _free_cache_entries(self):
        for key in random.sample(list(self._CACHE.keys()), min(len(self._CACHE), 16)):
            self._CACHE.pop(key, None)

    @classmethod
    def purge(cls):
        """Forget every parsed expression this parser has kept."""
        cls._CACHE.clear()

    def _advance(self):
        self._index += 1

    def _current_token(self):
        return self._tokens[self._index]["type"]

    def _lookahead(self, number):
        return self._tokens[self._index + number]["type"]

    def _lookahead_token(self, number):
        return self._tokens[self._index + number]


class ParsedResult:
    """One parsed expression, searched as many times as a caller likes."""

    def __init__(self, expression, parsed):
        self.expression = expression
        self.parsed = parsed

    def search(self, value, options=None):
        interpreter = TreeInterpreter(options)
        result = interpreter.visit(self.parsed, value)
        return result

    def _render_dot_file(self):
        """The parsed expression as the graph the library renders one as."""
        renderer = GraphvizVisitor()
        contents = renderer.visit(self.parsed)
        return contents

    def __repr__(self):
        return repr(self.parsed)
"#;

/// `jmespath.functions`: the functions the language defines, and the table a caller's own are
/// added to.
const FUNCTIONS_SOURCE: &str = r#"
"""The functions an expression may call."""

import json
import math

from jmespath import exceptions

# What each type of the language is called, read off the Python type of a value: these are the
# names the specification uses, and the ones a type error reports.
TYPES_MAP = {
    "bool": "boolean",
    "list": "array",
    "dict": "object",
    "NoneType": "null",
    "str": "string",
    "float": "number",
    "int": "number",
    "OrderedDict": "object",
    "_Projection": "array",
    "_Expression": "expref",
}

REVERSE_TYPES_MAP = {
    "boolean": ("bool",),
    "array": ("list", "_Projection"),
    "object": ("dict", "OrderedDict"),
    "null": ("NoneType",),
    "string": ("str",),
    "number": ("float", "int"),
    "expref": ("_Expression",),
}


def signature(*arguments):
    """The ARITY and the types one function takes, which every call of it is checked
    against."""

    def _record_signature(function):
        function.signature = arguments
        return function

    return _record_signature


class FunctionRegistry(type):
    """Builds the table of functions a class declares, so a subclass that declares one more
    has it without a registration step of its own."""

    def __init__(cls, name, bases, attrs):
        cls._populate_function_table()
        super().__init__(name, bases, attrs)

    def _populate_function_table(cls):
        function_table = dict(getattr(cls, "FUNCTION_TABLE", {}))
        for name, method in cls.__dict__.items():
            if not name.startswith("_func_"):
                continue
            signature = getattr(method, "signature", None)
            if signature is None:
                continue
            function_table[name[len("_func_") :]] = {
                "function": method,
                "signature": signature,
            }
        cls.FUNCTION_TABLE = function_table


class Functions(metaclass=FunctionRegistry):
    """The functions the language defines.

    A caller adds its own by subclassing this and declaring a `_func_<name>` method with the
    `signature` of the arguments it takes, which is how the published library is extended.
    """

    FUNCTION_TABLE = {}

    def call_function(self, function_name, resolved_args):
        try:
            spec = self.FUNCTION_TABLE[function_name]
        except KeyError:
            raise exceptions.UnknownFunctionError(
                "Unknown function: %s()" % function_name
            )
        function = spec["function"]
        signature = spec["signature"]
        self._validate_arguments(resolved_args, signature, function_name)
        return function(self, *resolved_args)

    def _validate_arguments(self, args, signature, function_name):
        if signature and signature[-1].get("variadic"):
            if len(args) < len(signature):
                raise exceptions.VariadictArityError(
                    len(signature), len(args), function_name
                )
        elif len(args) != len(signature):
            raise exceptions.ArityError(len(signature), len(args), function_name)
        return self._type_check(args, signature, function_name)

    def _type_check(self, actual, signature, function_name):
        for i in range(len(signature)):
            allowed_types = signature[i]["types"]
            if allowed_types:
                self._type_check_single(actual[i], allowed_types, function_name)

    def _type_check_single(self, current, types, function_name):
        # A type of the language is read off the PYTHON type of the value rather than with
        # `isinstance`, because a boolean is a subclass of an integer there and is no number
        # here. An entry naming a list of one type (`array-number`) admits an array, and
        # then says what its elements have to be.
        actual_type = TYPES_MAP.get(type(current).__name__, "unknown")
        allowed = []
        subtypes = []
        for entry in types:
            if entry.startswith("array-"):
                if "array" not in allowed:
                    allowed.append("array")
                subtypes.append(entry[len("array-") :])
            elif entry not in allowed:
                allowed.append(entry)
        if actual_type not in allowed:
            raise exceptions.JMESPathTypeError(
                function_name, current, actual_type, types
            )
        if actual_type == "array" and subtypes:
            self._subtype_check(current, subtypes, types, function_name)
        return True

    def _subtype_check(self, current, subtypes, types, function_name):
        """Every element of `current` is of one of the element types `subtypes` names.

        Where a single element type is allowed, each element is checked against it; where
        several are, every element has to be of the SAME one of them, which is what an
        entry admitting an array of numbers or an array of strings means.
        """
        if len(subtypes) == 1:
            for element in current:
                actual_element = TYPES_MAP.get(type(element).__name__, "unknown")
                if actual_element != subtypes[0]:
                    raise exceptions.JMESPathTypeError(
                        function_name, element, actual_element, types
                    )
            return
        for wanted in subtypes:
            matched = True
            for element in current:
                if TYPES_MAP.get(type(element).__name__, "unknown") != wanted:
                    matched = False
                    break
            if matched:
                return
        raise exceptions.JMESPathTypeError(function_name, current, "array", types)

    @signature({"types": ["number"]})
    def _func_abs(self, arg):
        return abs(arg)

    @signature({"types": ["array-number"]})
    def _func_avg(self, values):
        if not values:
            return None
        return sum(values) / float(len(values))

    @signature({"types": [], "variadic": True})
    def _func_not_null(self, *arguments):
        for argument in arguments:
            if argument is not None:
                return argument
        return None

    @signature({"types": []})
    def _func_to_array(self, arg):
        if isinstance(arg, list):
            return arg
        return [arg]

    @signature({"types": []})
    def _func_to_string(self, arg):
        if isinstance(arg, str):
            return arg
        return json.dumps(arg, separators=(",", ":"), default=str)

    @signature({"types": []})
    def _func_to_number(self, arg):
        if isinstance(arg, (list, dict, bool)):
            return None
        if arg is None:
            return None
        if isinstance(arg, (int, float)):
            return arg
        try:
            if "." not in arg:
                return int(arg)
            return float(arg)
        except ValueError:
            return None

    @signature({"types": ["array", "string"]}, {"types": []})
    def _func_contains(self, subject, search):
        return search in subject

    @signature({"types": ["string", "array", "object"]})
    def _func_length(self, arg):
        return len(arg)

    @signature({"types": ["string"]}, {"types": ["string"]})
    def _func_ends_with(self, search, suffix):
        return search.endswith(suffix)

    @signature({"types": ["string"]}, {"types": ["string"]})
    def _func_starts_with(self, search, suffix):
        return search.startswith(suffix)

    @signature({"types": ["array", "string"]})
    def _func_reverse(self, arg):
        if isinstance(arg, str):
            return arg[::-1]
        return list(reversed(arg))

    @signature({"types": ["number"]})
    def _func_ceil(self, arg):
        return math.ceil(arg)

    @signature({"types": ["number"]})
    def _func_floor(self, arg):
        return math.floor(arg)

    @signature({"types": ["string"]}, {"types": ["array-string"]})
    def _func_join(self, separator, array):
        return separator.join(array)

    @signature({"types": ["expref"]}, {"types": ["array"]})
    def _func_map(self, expref, arg):
        result = []
        for element in arg:
            result.append(expref.visit(expref.expression, element))
        return result

    @signature({"types": ["array-number", "array-string"]})
    def _func_max(self, arg):
        if arg:
            return max(arg)
        return None

    @signature({"types": ["object"], "variadic": True})
    def _func_merge(self, *arguments):
        merged = {}
        for arg in arguments:
            merged.update(arg)
        return merged

    @signature({"types": ["array-number", "array-string"]})
    def _func_min(self, arg):
        if arg:
            return min(arg)
        return None

    @signature({"types": ["array-string", "array-number"]})
    def _func_sort(self, arg):
        return list(sorted(arg))

    @signature({"types": ["array-number", "array-string"]})
    def _func_sum(self, arg):
        return sum(arg)

    @signature({"types": ["object"]})
    def _func_keys(self, arg):
        return list(arg.keys())

    @signature({"types": ["object"]})
    def _func_values(self, arg):
        return list(arg.values())

    @signature({"types": []})
    def _func_type(self, arg):
        if isinstance(arg, str):
            return "string"
        if isinstance(arg, bool):
            return "boolean"
        if isinstance(arg, list):
            return "array"
        if isinstance(arg, dict):
            return "object"
        if arg is None:
            return "null"
        if isinstance(arg, (float, int)):
            return "number"
        return "unknown"

    @signature({"types": ["array"]}, {"types": ["expref"]})
    def _func_sort_by(self, array, expref):
        if not array:
            return array
        # The key every element is sorted by has to be of ONE comparable type, which is what
        # the published library requires of it.
        required_type = self._convert_to_jmespath_type(
            type(expref.visit(expref.expression, array[0])).__name__
        )
        if required_type not in ["number", "string"]:
            raise exceptions.JMESPathTypeError(
                "sort_by", array[0], required_type, ["number", "string"]
            )

        def keyfunc(element):
            result = expref.visit(expref.expression, element)
            actual_type = self._convert_to_jmespath_type(type(result).__name__)
            if actual_type != required_type:
                raise exceptions.JMESPathTypeError(
                    "sort_by", result, actual_type, [required_type]
                )
            return result

        return list(sorted(array, key=keyfunc))

    @signature({"types": ["array"]}, {"types": ["expref"]})
    def _func_min_by(self, array, expref):
        keyfunc = self._create_key_func(expref, ["number", "string"], "min_by")
        if keyfunc is None:
            return None
        return min(array, key=keyfunc)

    @signature({"types": ["array"]}, {"types": ["expref"]})
    def _func_max_by(self, array, expref):
        keyfunc = self._create_key_func(expref, ["number", "string"], "max_by")
        if keyfunc is None:
            return None
        return max(array, key=keyfunc)

    def _create_key_func(self, expref, allowed_types, function_name):
        def keyfunc(element):
            result = expref.visit(expref.expression, element)
            actual_type = self._convert_to_jmespath_type(type(result).__name__)
            if actual_type not in allowed_types:
                raise exceptions.JMESPathTypeError(
                    function_name, result, actual_type, allowed_types
                )
            return result

        return keyfunc

    def _convert_to_jmespath_type(self, pyobject):
        return TYPES_MAP.get(pyobject, "unknown")
"#;

/// `jmespath.visitor`: how a parsed expression is EVALUATED against the data.
const VISITOR_SOURCE: &str = r#"
"""How a parsed expression is evaluated against the data."""

import operator

from jmespath import functions
from jmespath.exceptions import JMESPathTypeError


def _equals(first, second):
    """Whether two values are the same value of the language.

    `true` is not `1` here, which is what the specification requires of a comparison: the
    types have to match before the values do.
    """
    if _is_special_number_case(first, second):
        return False
    return first == second


def _is_special_number_case(first, second):
    # A boolean is not a number, however Python compares the two.
    if isinstance(first, bool) ^ isinstance(second, bool):
        return _is_actual_number(first) or _is_actual_number(second)
    return False


def _is_comparable(value):
    return _is_actual_number(value)


def _is_actual_number(value):
    if isinstance(value, bool):
        return False
    return isinstance(value, (int, float))


class Options:
    """What a search is run with: the dictionary class a multi-select builds, and the
    functions a call is dispatched through."""

    def __init__(self, dict_cls=None, custom_functions=None, enable_legacy_literals=False):
        self.dict_cls = dict_cls
        self.custom_functions = custom_functions
        self.enable_legacy_literals = enable_legacy_literals


class _Expression:
    """An expression a function was handed, which it evaluates itself (`&expr`)."""

    def __init__(self, expression, interpreter):
        self.expression = expression
        self.interpreter = interpreter

    def visit(self, node, *args, **kwargs):
        return self.interpreter.visit(node, *args, **kwargs)


class Visitor:
    """Walks a parsed expression, node by node."""

    def __init__(self):
        self._method_cache = {}

    def visit(self, node, *args, **kwargs):
        node_type = node["type"]
        method = self._method_cache.get(node_type)
        if method is None:
            method = getattr(self, "visit_%s" % node["type"], self.default_visit)
            self._method_cache[node_type] = method
        return method(node, *args, **kwargs)

    def default_visit(self, node, *args, **kwargs):
        raise NotImplementedError("default_visit")


class TreeInterpreter(Visitor):
    """Evaluates a parsed expression against the data."""

    COMPARATOR_FUNC = {
        "lte": operator.le,
        "lt": operator.lt,
        "gte": operator.ge,
        "gt": operator.gt,
        "eq": _equals,
        "ne": lambda first, second: not _equals(first, second),
    }

    _EQUALITY_OPS = ["eq", "ne"]

    class _Projection(list):
        """The result of a projection, which is an array that KEEPS projecting."""

        def __init__(self, elements):
            list.__init__(self, elements)

    def __init__(self, options=None):
        super().__init__()
        self._options = options if options is not None else Options()
        self._dict_cls = self._options.dict_cls if self._options.dict_cls else dict
        if self._options.custom_functions is None:
            self._functions = functions.Functions()
        else:
            self._functions = self._options.custom_functions

    def default_visit(self, node, *args, **kwargs):
        raise NotImplementedError(node["type"])

    def visit_subexpression(self, node, value):
        result = value
        for child in node["children"]:
            result = self.visit(child, result)
        return result

    def visit_field(self, node, value):
        try:
            return value.get(node["value"])
        except AttributeError:
            return None

    def visit_comparator(self, node, value):
        comparator_func = self.COMPARATOR_FUNC[node["value"]]
        if node["value"] in self._EQUALITY_OPS:
            return comparator_func(
                self.visit(node["children"][0], value),
                self.visit(node["children"][1], value),
            )
        # An ordering comparison is only of two numbers; anything else reaches nothing,
        # which is what the specification says of it.
        left = self.visit(node["children"][0], value)
        right = self.visit(node["children"][1], value)
        if not (_is_comparable(left) and _is_comparable(right)):
            return None
        return comparator_func(left, right)

    def visit_current(self, node, value):
        return value

    def visit_expref(self, node, value):
        return _Expression(node["children"][0], self)

    def visit_function_expression(self, node, value):
        resolved_args = []
        for child in node["children"]:
            current = self.visit(child, value)
            resolved_args.append(current)
        return self._functions.call_function(node["value"], resolved_args)

    def visit_filter_projection(self, node, value):
        base = self.visit(node["children"][0], value)
        if not isinstance(base, list):
            return None
        comparator_node = node["children"][2]
        collected = []
        for element in base:
            if self._is_true(self.visit(comparator_node, element)):
                current = self.visit(node["children"][1], element)
                if current is not None:
                    collected.append(current)
        return self._Projection(collected)

    def visit_flatten(self, node, value):
        base = self.visit(node["children"][0], value)
        if not isinstance(base, list):
            # A flatten of anything that is not an array reaches nothing.
            return None
        merged_list = []
        for element in base:
            if isinstance(element, list):
                merged_list.extend(element)
            else:
                merged_list.append(element)
        return self._Projection(merged_list)

    def visit_identity(self, node, value):
        return value

    def visit_index(self, node, value):
        # An index is only of an array, and a negative index counts from the end.
        if not isinstance(value, list):
            return None
        try:
            return value[node["value"]]
        except IndexError:
            return None

    def visit_index_expression(self, node, value):
        result = value
        for child in node["children"]:
            result = self.visit(child, result)
        return result

    def visit_slice(self, node, value):
        if isinstance(value, str):
            return value[slice(*node["children"])]
        if not isinstance(value, list):
            return None
        return self._Projection(value[slice(*node["children"])])

    def visit_key_val_pair(self, node, value):
        return self.visit(node["children"][0], value)

    def visit_literal(self, node, value):
        return node["value"]

    def visit_multi_select_dict(self, node, value):
        if value is None:
            return None
        collected = self._dict_cls()
        for child in node["children"]:
            collected[child["value"]] = self.visit(child, value)
        return collected

    def visit_multi_select_list(self, node, value):
        if value is None:
            return None
        collected = []
        for child in node["children"]:
            collected.append(self.visit(child, value))
        return collected

    def visit_or_expression(self, node, value):
        matched = self.visit(node["children"][0], value)
        if self._is_false(matched):
            matched = self.visit(node["children"][1], value)
        return matched

    def visit_and_expression(self, node, value):
        matched = self.visit(node["children"][0], value)
        if self._is_false(matched):
            return matched
        return self.visit(node["children"][1], value)

    def visit_not_expression(self, node, value):
        original_result = self.visit(node["children"][0], value)
        if _is_actual_number(original_result) and original_result == 0:
            # A zero is TRUE in this language, so its negation is false.
            return False
        return not original_result

    def visit_pipe(self, node, value):
        result = value
        for child in node["children"]:
            result = self.visit(child, result)
        return result

    def visit_projection(self, node, value):
        base = self.visit(node["children"][0], value)
        allow_string = False
        first_child = node["children"][0]
        if first_child["type"] == "index_expression":
            nested = first_child["children"][1]
            allow_string = nested["type"] == "slice"
        if isinstance(base, str) and allow_string:
            # A projection over a string is really a sub-expression: the right-hand side is
            # evaluated against the string itself.
            return self.visit(node["children"][1], base)
        if not isinstance(base, list):
            return None
        collected = []
        for element in base:
            current = self.visit(node["children"][1], element)
            if current is not None:
                collected.append(current)
        return self._Projection(collected)

    def visit_value_projection(self, node, value):
        base = self.visit(node["children"][0], value)
        try:
            base = base.values()
        except AttributeError:
            return None
        collected = []
        for element in base:
            current = self.visit(node["children"][1], element)
            if current is not None:
                collected.append(current)
        return self._Projection(collected)

    def _is_false(self, value):
        # The FALSE values of the language: null, an empty array, an empty object, an empty
        # string and false itself. A zero is not one of them.
        return value is None or value == "" or value == [] or value == {} or value is False

    def _is_true(self, value):
        return not self._is_false(value)


class GraphvizVisitor(Visitor):
    """A parsed expression as the graph the library renders one as."""

    def __init__(self):
        super().__init__()
        self._lines = []
        self._count = 1

    def visit(self, node, *args, **kwargs):
        self._lines.append("digraph AST {")
        current = "%s%s" % (node["type"], self._count)
        self._count += 1
        self._visit(node, current)
        self._lines.append("}")
        return "\n".join(self._lines)

    def _visit(self, node, current):
        self._lines.append(
            '%s [label="%s(%s)"]' % (current, node["type"], node.get("value", ""))
        )
        for child in node.get("children", []):
            child_name = "%s%s" % (child["type"], self._count)
            self._count += 1
            self._lines.append("  %s -> %s" % (current, child_name))
            self._visit(child, child_name)
"#;
