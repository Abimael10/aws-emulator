// SPDX-License-Identifier: CC0-1.0

//! The TEXT FILTERS a shell session performs: the utilities that write what they computed
//! from their input.
//!
//! # The rule this module implements
//!
//! A command of a build container, a managed node or a guest machine is written against the
//! text utilities the image provides, and each of them answers out of ONE input — the file it
//! was given, or its standard input — and writes what it computed over it. So what a pipeline
//! of them comes to follows from the bytes that went in and from nothing else: a digest is the
//! digest of those bytes, a count is their count, and a selection is the part of them the
//! option names.
//!
//! The utilities here are the ones whose whole effect is that computation, with the output
//! form each of them is specified to write:
//!
//! * `wc -c` / `-m` / `-l` / `-w` — the bytes, characters, newlines or words of its input, "one
//!   count only" when a single option selects one, and the name of the file after the count
//!   when it was given one;
//! * `cut -c LIST` / `-f LIST [-d DELIM]` — "select portions of each line of a file", the
//!   characters or the delimited fields the list names, one output line per input line;
//! * `head -n COUNT` — "the first COUNT lines of each file", ten lines when no count is given;
//! * `sha256sum` / `sha512sum` — the digest of its input, written as the lowercase hexadecimal
//!   digest, two spaces and the name of what was read (`-` for standard input);
//! * `base64` / `base64 -d` — "Base64 encode or decode FILE, or standard input, to standard
//!   output": the encoding wrapped after [`DEFAULT_BASE64_WRAP`] characters, or after the
//!   `-w COLS` an invocation names (`0` disabling the wrapping), and the decoding of its
//!   input, which "ignores" the line breaks an encoding wrote and, under `-i`, every other
//!   character outside the Base64 alphabet. Input that is no Base64 encoding at all is the
//!   utility's own [`BASE64_INVALID_INPUT`] failure.
//! * `grep [OPTION…] PATTERN [FILE]` — "print lines that match patterns": the lines of its
//!   input the pattern matches, inverted by `-v`, counted by `-c`, numbered by `-n`, reduced
//!   to the matching part by `-o`, written nowhere at all by `-q`, and matched without regard
//!   to case by `-i`. Its STATUS is the selection it made — "the exit status is 0 if a line is
//!   selected, 1 if no lines were selected" ([`Selection::selected`]) — which is what an
//!   `if grep …` of a program branches on. The pattern is matched by the maintained
//!   regular-expression engine rather than by anything written here: as a BASIC expression by
//!   default, as an EXTENDED one under `-E` ("interpret PATTERNS as extended regular
//!   expressions") and as a literal string under `-F` ("interpret PATTERNS as fixed strings").
//!
//! The option spellings and the output forms above are each utility's own, from the GNU
//! coreutils manual (`wc`, `cut`, `head`, `sha2 utilities`, `base64`) — which is what the images
//! these sessions stand for carry — and POSIX for `wc`, `cut` and `head`.
//!
//! # What is NOT performed
//!
//! Only forms whose output follows from the input alone, and ONE input at a time:
//!
//! * a `wc` with no count option writes three counts in a width it computes from the files it
//!   read, so that form is not performed rather than written in a width of this module's
//!   choosing;
//! * several file operands at once are not performed either: `wc` then writes a `total` line,
//!   `head` writes a header per file, `grep` prefixes every line with the file it came from,
//!   and all of them pad — none of which follows from one input;
//! * a `grep` whose BASIC pattern carries one of the escapes a basic expression gives its own
//!   meaning (`\(`, `\)`, `\{`, `\}`, `\|`, `\+`, `\?`) is not performed, rather than matched
//!   as though that escape meant what an extended expression means by it; nor is one asking
//!   for the lines AROUND a match, for the files rather than the lines, or for a walk of a
//!   directory, none of which follows from one input either;
//! * every other utility, option and operand form answers as a command the session could not
//!   run ([`Filter::read`] answering `None`), which is the fail-closed default the session's
//!   own catalog keeps.
//!
//! A session's streams and files carry TEXT, so a `base64 -d` whose bytes are not text writes
//! none of them and fails ([`BASE64_UNDECODABLE_BYTES`]) rather than writing something other
//! than what the input decodes to.
//!
//! This module computes over the text it is handed: it opens nothing, reads no path of the
//! host, and holds nothing between calls. WHERE the input comes from — a file of the session's
//! tree or the stage before it in a pipeline — is the session's to resolve
//! ([`crate::posix_shell_session`]).

use ::sha2::Digest;

/// The utilities this module performs, as the names a command line invokes them by. A session
/// hands a command whose name is one of these to [`Filter::read`].
pub const TEXT_FILTER_COMMANDS: &[&str] =
    &["wc", "cut", "head", "sha256sum", "sha512sum", "base64", GREP_COMMAND];

/// The utility of [`TEXT_FILTER_COMMANDS`] whose STATUS is the selection it made, which is
/// the one a program's `if` branches on.
const GREP_COMMAND: &str = "grep";

/// Which count a `wc` invocation asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
    /// `-c`: "the number of bytes".
    Bytes,
    /// `-m`: "the number of characters".
    Characters,
    /// `-l`: "the number of newlines".
    Lines,
    /// `-w`: "the number of words", a word being "a nonempty sequence of characters delimited
    /// by white space".
    Words,
}

/// Which digest a checksum invocation writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Digester {
    /// `sha256sum`.
    Sha256,
    /// `sha512sum`.
    Sha512,
}

/// What one `cut` invocation selects out of each line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Portion {
    /// `-c LIST`: the characters the list names.
    Characters(Ranges),
    /// `-f LIST [-d DELIM]`: the delimited fields the list names.
    Fields {
        /// The fields selected.
        ranges: Ranges,
        /// The delimiter, which is a TAB when none was given.
        delimiter: char,
    },
}

/// What one `base64` invocation asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Base64Work {
    /// `-d`/`--decode`: "decode data" rather than encode it.
    decode: bool,
    /// `-i`/`--ignore-garbage`: "when decoding, ignore non-alphabet characters".
    ignore_garbage: bool,
    /// `-w`/`--wrap=COLS`: "wrap encoded lines after COLS character", and `0` to "disable line
    /// wrapping".
    wrap: usize,
}

/// What one `grep` invocation selects, and how it writes it.
///
/// The pattern is held as the SOURCE the regular-expression engine reads, translated once out
/// of the expression flavour the invocation asked for ([`grep_pattern`]); it is compiled when
/// the invocation is performed, over the input it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GrepWork {
    /// The pattern, in the syntax the engine reads.
    pattern: String,
    /// `-i`/`--ignore-case`: "ignore case distinctions in patterns and data".
    ignore_case: bool,
    /// `-v`/`--invert-match`: "select non-matching lines".
    invert: bool,
    /// `-c`/`--count`: "print only a count of selected lines per FILE".
    count: bool,
    /// `-q`/`--quiet`/`--silent`: "suppress all normal output", leaving only the status.
    quiet: bool,
    /// `-o`/`--only-matching`: "print only the matched (non-empty) parts of a matching line".
    only_matching: bool,
    /// `-n`/`--line-number`: "print line number with output lines".
    line_number: bool,
}

/// The work one invocation of a text filter performs over ONE input.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Work {
    /// `wc`, counting one way.
    Count(Count),
    /// `cut`, selecting one portion of each line.
    Cut(Portion),
    /// `head`, keeping the first lines.
    Head(usize),
    /// `sha256sum` / `sha512sum`.
    Digest(Digester),
    /// `base64`, encoding or decoding.
    Base64(Base64Work),
    /// `grep`, selecting the lines its pattern matches.
    Grep(GrepWork),
}

/// What one invocation of a filter came to: what it writes to standard output, and the
/// SELECTION its status follows from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// What the utility writes to standard output, which may be nothing at all.
    pub written: String,
    /// Whether the utility SELECTED anything, for the utility whose status says so: "the exit
    /// status is 0 if a line is selected, 1 if no lines were selected" (`grep`). `None` for
    /// every utility whose status is success whenever it could compute at all.
    pub selected: Option<bool>,
}

impl Selection {
    /// A utility that wrote `written` and whose status does not report a selection.
    fn wrote(written: String) -> Self {
        Self {
            written,
            selected: None,
        }
    }
}

/// One invocation of a text filter: the work it performs, and the file it reads when it names
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// What this invocation computes.
    work: Work,
    /// The file operand, and `None` for an invocation that reads standard input.
    file: Option<String>,
}

/// How many lines `head` writes when the invocation names no count: "head will print the first
/// 10 lines".
const DEFAULT_HEAD_LINES: usize = 10;

/// The delimiter `cut -f` takes when the invocation names none: "the default is TAB".
const DEFAULT_FIELD_DELIMITER: char = '\t';

/// How many characters an ENCODING is wrapped after when the invocation names no width:
/// "wrap encoded lines after COLS character (default 76)".
pub const DEFAULT_BASE64_WRAP: usize = 76;

/// What `base64` writes to standard error for input that is no Base64 encoding: "base64:
/// invalid input".
pub const BASE64_INVALID_INPUT: &str = "base64: invalid input";

/// What a `base64 -d` writes to standard error when what its input decodes to is not text,
/// which a session's streams and files carry (see the module docs).
pub const BASE64_UNDECODABLE_BYTES: &str =
    "base64: the input decodes to bytes that are not text";

/// What the output names for an input read from standard input, as a checksum utility writes
/// it.
pub const STANDARD_INPUT_LABEL: &str = "-";

impl Filter {
    /// The invocation `name` with `arguments` performs, or `None` for a utility, option or
    /// operand form this module does not perform (see the module docs).
    #[must_use]
    pub fn read(name: &str, arguments: &[String]) -> Option<Self> {
        // The options that carry a VALUE take it either glued to them (`-c1-64`) or as the
        // word after them (`-c 1-64`), and a utility takes both spellings; the second form is
        // folded into the first here, so what follows reads one shape — and a delimiter that
        // is a space or a digit is that option's value rather than an operand.
        let value_taking = value_taking_options(name);
        let mut options: Vec<String> = Vec::new();
        let mut operands: Vec<String> = Vec::new();
        let mut taken = arguments.iter();
        while let Some(argument) = taken.next() {
            if argument.len() > 1 && argument.starts_with('-') {
                if value_taking.contains(&argument.as_str()) {
                    // An option whose value the invocation never wrote is not a form the
                    // utility performs either.
                    options.push(format!("{argument}{}", taken.next()?));
                } else {
                    options.push(argument.clone());
                }
                continue;
            }
            operands.push(argument.clone());
        }
        // `grep` reads its OWN operands: the first of them is the pattern unless an option
        // carried one, and the file comes after it.
        if name == GREP_COMMAND {
            return grep_filter(&options, &operands);
        }
        // ONE input at a time: several files make an output this module does not write (see the
        // module docs), and `-` is the spelling for the standard input it would read anyway.
        let file = match operands.len() {
            0 => None,
            1 => operands
                .first()
                .filter(|operand| operand.as_str() != STANDARD_INPUT_LABEL)
                .cloned(),
            _ => return None,
        };
        let work = match name {
            "wc" => Work::Count(count_of(&options)?),
            "cut" => Work::Cut(portion_of(&options)?),
            "head" => Work::Head(head_lines(&options)?),
            "sha256sum" => Work::Digest(Digester::Sha256),
            "sha512sum" => Work::Digest(Digester::Sha512),
            "base64" => Work::Base64(base64_work(&options)?),
            _ => return None,
        };
        Some(Self { work, file })
    }

    /// The file this invocation reads, and `None` for one that reads standard input.
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// What this invocation writes to standard output for `input`, which was read from
    /// `label` — the file's own name, or [`STANDARD_INPUT_LABEL`] for standard input — beside
    /// the selection its status follows from ([`Selection`]).
    ///
    /// `Err` is what the utility writes to standard ERROR for an input it cannot compute over,
    /// which is a failed invocation rather than one that wrote nothing.
    pub fn perform(&self, label: &str, input: &str) -> Result<Selection, String> {
        Ok(match &self.work {
            Work::Count(count) => {
                let counted = counted(*count, input);
                // The count alone for standard input; the count and the file's name for a file
                // operand, which is the form the utility writes for one.
                Selection::wrote(match self.file.as_deref() {
                    Some(_) => format!("{counted} {label}\n"),
                    None => format!("{counted}\n"),
                })
            }
            Work::Cut(portion) => Selection::wrote(cut_lines(portion, input)),
            Work::Head(lines) => Selection::wrote(head_of(*lines, input)),
            Work::Digest(digester) => {
                Selection::wrote(format!("{}  {label}\n", digest_of(*digester, input)))
            }
            Work::Base64(work) => Selection::wrote(base64_performed(*work, input)?),
            Work::Grep(work) => grep_performed(work, input)?,
        })
    }
}

/// The options of `name` that carry a VALUE, which an invocation may write glued to them or
/// as the word after them ([`Filter::read`] folds the second spelling into the first).
fn value_taking_options(name: &str) -> &'static [&'static str] {
    match name {
        // `cut`'s lists and its delimiter.
        "cut" => &["-c", "-f", "-d"],
        // `head`'s line count.
        "head" => &["-n"],
        // `base64`'s wrapping width.
        "base64" => &["-w", "--wrap"],
        // `grep`'s pattern, for the invocation that names it with an option rather than as its
        // first operand.
        GREP_COMMAND => &["-e", "--regexp"],
        // `wc`'s counts and the checksum utilities take none.
        _ => &[],
    }
}

/// The count a `wc` invocation's options select, or `None` when they select none or more than
/// one — the forms whose output does not follow from one input (see the module docs).
fn count_of(options: &[String]) -> Option<Count> {
    let mut selected = None;
    for option in options {
        let count = match option.as_str() {
            "-c" | "--bytes" => Count::Bytes,
            "-m" | "--chars" => Count::Characters,
            "-l" | "--lines" => Count::Lines,
            "-w" | "--words" => Count::Words,
            _ => return None,
        };
        if selected.is_some_and(|held| held != count) {
            return None;
        }
        selected = Some(count);
    }
    selected
}

/// What `wc` answers for `input` under `count`.
fn counted(count: Count, input: &str) -> usize {
    match count {
        Count::Bytes => input.len(),
        Count::Characters => input.chars().count(),
        // "the number of newlines", which is what the utility counts — a last line with no
        // newline after it is not counted.
        Count::Lines => input.matches('\n').count(),
        Count::Words => input.split_whitespace().count(),
    }
}

/// The portion a `cut` invocation's options select, or `None` for an invocation that selects
/// none, selects both kinds at once, or carries an option this module does not perform.
///
/// Every option here carries its value glued to it ([`Filter::read`]).
fn portion_of(options: &[String]) -> Option<Portion> {
    let mut characters = None;
    let mut fields = None;
    let mut delimiter = None;
    for option in options {
        if let Some(list) = non_empty(option.strip_prefix("-c")) {
            characters = Some(ranges_of(list)?);
            continue;
        }
        if let Some(list) = non_empty(option.strip_prefix("-f")) {
            fields = Some(ranges_of(list)?);
            continue;
        }
        if let Some(written) = non_empty(option.strip_prefix("-d")) {
            // One character, which is what the utility's delimiter is.
            let mut written = written.chars();
            let only = written.next()?;
            if written.next().is_some() {
                return None;
            }
            delimiter = Some(only);
            continue;
        }
        return None;
    }
    match (characters, fields) {
        (Some(ranges), None) if delimiter.is_none() => Some(Portion::Characters(ranges)),
        (None, Some(ranges)) => Some(Portion::Fields {
            ranges,
            delimiter: delimiter.unwrap_or(DEFAULT_FIELD_DELIMITER),
        }),
        _ => None,
    }
}

/// `value` when an option carried one, and `None` for an option that is not this one or that
/// carried nothing.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// How many lines a `head` invocation keeps, or `None` for an option form this module does not
/// perform.
fn head_lines(options: &[String]) -> Option<usize> {
    let mut lines = None;
    for option in options {
        let written = non_empty(option.strip_prefix("-n"))?;
        lines = Some(written.trim().parse::<usize>().ok()?);
    }
    Some(lines.unwrap_or(DEFAULT_HEAD_LINES))
}

/// What a `base64` invocation's options ask for, or `None` for an option form this module does
/// not perform.
///
/// The width option carries its value glued to it ([`Filter::read`] folds the spelling that
/// writes it as the next word into that one), written either way GNU writes it (`-w COLS`,
/// `--wrap=COLS`).
fn base64_work(options: &[String]) -> Option<Base64Work> {
    let mut work = Base64Work {
        decode: false,
        ignore_garbage: false,
        wrap: DEFAULT_BASE64_WRAP,
    };
    for option in options {
        match option.as_str() {
            "-d" | "--decode" => work.decode = true,
            "-i" | "--ignore-garbage" => work.ignore_garbage = true,
            _ => {
                let written = option
                    .strip_prefix("--wrap")
                    .map(|rest| rest.strip_prefix('=').unwrap_or(rest))
                    .or_else(|| option.strip_prefix("-w"))
                    .filter(|written| !written.is_empty())?;
                work.wrap = written.trim().parse::<usize>().ok()?;
            }
        }
    }
    Some(work)
}

/// What a `base64` invocation writes for `input`, and the standard-error line of one whose
/// input it cannot compute over.
fn base64_performed(work: Base64Work, input: &str) -> Result<String, String> {
    if work.decode {
        return base64_decoded(work, input);
    }
    Ok(base64_encoded(work, input))
}

/// The Base64 encoding of `input`, wrapped as the invocation asks.
///
/// "Wrap encoded lines after COLS character", so every line but the last is exactly that wide
/// and each is ended by the newline the utility writes; a width of `0` disables the wrapping
/// and the whole encoding is one line. An EMPTY input encodes to nothing, so nothing is
/// written for it — not even a line ending.
fn base64_encoded(work: Base64Work, input: &str) -> String {
    use ::base64::Engine as _;

    let encoded = ::base64::engine::general_purpose::STANDARD.encode(input.as_bytes());
    if encoded.is_empty() {
        return String::new();
    }
    if work.wrap == 0 || encoded.len() <= work.wrap {
        return format!("{encoded}\n");
    }
    // The encoding is Base64 alphabet, so it is ASCII and a byte boundary is a character
    // boundary.
    let mut written = String::with_capacity(encoded.len() + encoded.len() / work.wrap + 1);
    for line in encoded.as_bytes().chunks(work.wrap) {
        written.push_str(&String::from_utf8_lossy(line));
        written.push('\n');
    }
    written
}

/// What `input` decodes to, or the standard-error line of a decode that could not be made.
///
/// The line breaks an encoding wrote are not data, so they are not decoded as any: GNU's own
/// decoder ignores them, and `-i` ("ignore non-alphabet characters") extends that to every
/// character outside the alphabet. What is left has to be a Base64 encoding — anything else is
/// [`BASE64_INVALID_INPUT`] — and what it decodes to has to be text, which is what a session's
/// streams and files carry ([`BASE64_UNDECODABLE_BYTES`]).
fn base64_decoded(work: Base64Work, input: &str) -> Result<String, String> {
    use ::base64::Engine as _;

    let held: String = input
        .chars()
        .filter(|character| {
            if work.ignore_garbage {
                is_base64_alphabet(*character)
            } else {
                *character != '\n' && *character != '\r'
            }
        })
        .collect();
    let decoded = ::base64::engine::general_purpose::STANDARD
        .decode(held.as_bytes())
        .map_err(|_| BASE64_INVALID_INPUT.to_string())?;
    String::from_utf8(decoded).map_err(|_| BASE64_UNDECODABLE_BYTES.to_string())
}

/// Whether `character` is one of the Base64 alphabet or its padding, which is what a decode
/// reads and what `-i` keeps of its input (RFC 4648 table 1 and the `=` pad).
fn is_base64_alphabet(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '=')
}

/// The flavour of regular expression a `grep` invocation's pattern is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavour {
    /// The default: "interpret PATTERNS as basic regular expressions".
    Basic,
    /// `-E`: "interpret PATTERNS as extended regular expressions".
    Extended,
    /// `-F`: "interpret PATTERNS as fixed strings".
    Fixed,
}

/// The characters a BASIC regular expression gives no special meaning to, and which the
/// engine's own syntax does: a basic pattern that carries one carries it as a literal, so it is
/// escaped before the engine reads it.
const LITERAL_IN_BASIC: [char; 7] = ['+', '?', '|', '(', ')', '{', '}'];

/// The escapes a BASIC regular expression gives ITS OWN meaning to, which is the opposite of
/// what the engine reads them as. A pattern carrying one is not performed (see the module
/// docs) rather than matched as something else.
const BASIC_ESCAPES: [&str; 7] = [r"\(", r"\)", r"\{", r"\}", r"\|", r"\+", r"\?"];

/// The `grep` invocation `options` and `operands` state, or `None` for an option, operand or
/// combination this module does not perform (see the module docs).
///
/// The pattern is the one an option named (`-e PATTERN`, `--regexp=PATTERN`), or the FIRST
/// operand when no option named one — which is how the utility's own usage reads it — and the
/// one file operand after it is the input.
fn grep_filter(options: &[String], operands: &[String]) -> Option<Filter> {
    let mut work = GrepWork {
        pattern: String::new(),
        ignore_case: false,
        invert: false,
        count: false,
        quiet: false,
        only_matching: false,
        line_number: false,
    };
    let mut flavour = Flavour::Basic;
    let mut written: Option<String> = None;
    for option in options {
        if let Some(pattern) = long_pattern(option) {
            // ONE pattern: several of them select out of one input in a way this module does
            // not write.
            if written.replace(pattern.to_string()).is_some() {
                return None;
            }
            continue;
        }
        if let Some(name) = option.strip_prefix("--") {
            match name {
                "ignore-case" => work.ignore_case = true,
                "invert-match" => work.invert = true,
                "count" => work.count = true,
                "quiet" | "silent" => work.quiet = true,
                "only-matching" => work.only_matching = true,
                "line-number" => work.line_number = true,
                "extended-regexp" => flavour = Flavour::Extended,
                "fixed-strings" => flavour = Flavour::Fixed,
                "basic-regexp" => flavour = Flavour::Basic,
                _ => return None,
            }
            continue;
        }
        // Several valueless flags written as one word, as `-qv` is.
        for flag in option.strip_prefix('-')?.chars() {
            match flag {
                'i' => work.ignore_case = true,
                'v' => work.invert = true,
                'c' => work.count = true,
                'q' => work.quiet = true,
                'o' => work.only_matching = true,
                'n' => work.line_number = true,
                'E' => flavour = Flavour::Extended,
                'F' => flavour = Flavour::Fixed,
                'G' => flavour = Flavour::Basic,
                _ => return None,
            }
        }
    }
    // "print only the matched parts of a matching line" has nothing to write for a line that
    // was selected because it did NOT match, so the two together are not a form the utility
    // performs.
    if work.only_matching && work.invert {
        return None;
    }
    let mut operands = operands.iter();
    let pattern = match written {
        Some(pattern) => pattern,
        None => operands.next()?.clone(),
    };
    work.pattern = grep_pattern(&pattern, flavour)?;
    let file = operands
        .next()
        .filter(|operand| operand.as_str() != STANDARD_INPUT_LABEL)
        .cloned();
    // ONE input at a time, like every other utility here.
    if operands.next().is_some() {
        return None;
    }
    // A pattern the engine cannot read is an invocation this module performs nothing of,
    // rather than one that selects nothing.
    grep_expression(&work)?;
    Some(Filter {
        work: Work::Grep(work),
        file,
    })
}

/// The pattern a LONG option named, in either spelling the utility takes it in — and the one a
/// `-e` carried, which [`Filter::read`] has already glued to the option.
fn long_pattern(option: &str) -> Option<&str> {
    if let Some(pattern) = option.strip_prefix("--regexp=") {
        return Some(pattern);
    }
    if option.starts_with("--") {
        return None;
    }
    non_empty(option.strip_prefix("-e"))
}

/// `written` as the pattern source the engine reads, for the flavour the invocation asked for,
/// or `None` for a pattern this module does not perform.
///
/// A FIXED string is escaped whole, so every character of it matches itself. An EXTENDED
/// expression is what the engine reads already. A BASIC one differs from it in exactly the
/// characters above: the ones it holds as literals are escaped ([`LITERAL_IN_BASIC`]), and one
/// carrying an escape whose meaning the two disagree on is not performed ([`BASIC_ESCAPES`]).
fn grep_pattern(written: &str, flavour: Flavour) -> Option<String> {
    match flavour {
        Flavour::Fixed => Some(::regex::escape(written)),
        Flavour::Extended => Some(written.to_string()),
        Flavour::Basic => {
            if BASIC_ESCAPES
                .iter()
                .any(|escape| written.contains(escape))
            {
                return None;
            }
            let mut source = String::with_capacity(written.len());
            let mut characters = written.chars();
            while let Some(character) = characters.next() {
                // An escape a basic expression and the engine agree on (`\.`, `\*`, `\\`) is
                // carried through as written, with the character it escapes.
                if character == '\\' {
                    source.push(character);
                    if let Some(escaped) = characters.next() {
                        source.push(escaped);
                    }
                    continue;
                }
                if LITERAL_IN_BASIC.contains(&character) {
                    source.push('\\');
                }
                source.push(character);
            }
            Some(source)
        }
    }
}

/// How large a compiled expression one `grep` pattern may produce, and how large the matcher's
/// own cache may grow for it.
///
/// CHOSEN, not a `grep` limit: the pattern is caller-supplied text, so what COMPILING one may
/// cost this process is bounded here rather than by the pattern's own length — a pattern past
/// it is one this module does not perform. The matching side needs no bound of its own: the
/// engine matches in time linear in the input, and the input is a session's file or stream,
/// bounded by that session ([`crate::posix_shell_session`]).
const MAX_PATTERN_BYTES: usize = 64 * 1024;

/// The expression `work`'s pattern compiles to, and `None` when the engine cannot read it or it
/// compiles past [`MAX_PATTERN_BYTES`].
fn grep_expression(work: &GrepWork) -> Option<::regex::Regex> {
    ::regex::RegexBuilder::new(&work.pattern)
        .case_insensitive(work.ignore_case)
        .size_limit(MAX_PATTERN_BYTES)
        .dfa_size_limit(MAX_PATTERN_BYTES)
        .build()
        .ok()
}

/// What a `grep` invocation writes for `input`, and the selection its status follows from.
///
/// "Print lines that match patterns": a line is SELECTED when the pattern matches it, or — for
/// `-v` — when it does not, and what is written is the selected lines themselves, their count
/// (`-c`), the matching parts alone (`-o`), each prefixed by its line number (`-n`), or nothing
/// at all (`-q`). The status is the selection, whatever was written.
fn grep_performed(work: &GrepWork, input: &str) -> Result<Selection, String> {
    let expression = grep_expression(work).ok_or_else(|| {
        format!(
            "{GREP_COMMAND}: {}: Invalid regular expression",
            work.pattern
        )
    })?;
    let mut written = String::new();
    let mut selected = 0_usize;
    for (index, line) in lines_of(input).iter().enumerate() {
        if expression.is_match(line) == work.invert {
            continue;
        }
        selected = selected.saturating_add(1);
        if work.quiet || work.count {
            continue;
        }
        let number = index.saturating_add(1);
        if work.only_matching {
            // "print only the matched (non-empty) parts of a matching line, with each such
            // part on a separate output line".
            for found in expression.find_iter(line).filter(|found| !found.is_empty()) {
                if work.line_number {
                    written.push_str(&format!("{number}:"));
                }
                written.push_str(found.as_str());
                written.push('\n');
            }
            continue;
        }
        if work.line_number {
            written.push_str(&format!("{number}:"));
        }
        written.push_str(line);
        written.push('\n');
    }
    if work.count && !work.quiet {
        written = format!("{selected}\n");
    }
    Ok(Selection {
        written,
        selected: Some(selected > 0),
    })
}

/// The positions one `cut` list names, as the ranges it is written with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ranges(Vec<(usize, Option<usize>)>);

impl Ranges {
    /// Whether the position `index` — ONE-based, as the utility's lists are written — is
    /// selected.
    fn holds(&self, index: usize) -> bool {
        self.0
            .iter()
            .any(|(low, high)| index >= *low && high.is_none_or(|high| index <= high))
    }
}

/// The ranges `list` names — `N`, `N-M`, `-M` and `N-`, separated by commas — or `None` for a
/// list this module does not read.
fn ranges_of(list: &str) -> Option<Ranges> {
    let mut ranges = Vec::new();
    for written in list.split(',') {
        let written = written.trim();
        if written.is_empty() {
            return None;
        }
        let range = match written.split_once('-') {
            // `N`: that one position.
            None => {
                let only = written.parse::<usize>().ok()?;
                (only, Some(only))
            }
            // `-M`: from the first position through M.
            Some(("", high)) => (1, Some(high.parse::<usize>().ok()?)),
            // `N-`: from N through the end of the line.
            Some((low, "")) => (low.parse::<usize>().ok()?, None),
            // `N-M`.
            Some((low, high)) => (low.parse::<usize>().ok()?, Some(high.parse::<usize>().ok()?)),
        };
        // A position is one-based, and a range whose end is before its start names nothing the
        // utility would select.
        if range.0 == 0 || range.1.is_some_and(|high| high < range.0) {
            return None;
        }
        ranges.push(range);
    }
    (!ranges.is_empty()).then_some(Ranges(ranges))
}

/// Every line of `input`, without the newline that ended it — a last line with no newline is
/// still a line.
fn lines_of(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }
    input
        .strip_suffix('\n')
        .unwrap_or(input)
        .split('\n')
        .collect()
}

/// `cut`'s selection out of each line of `input`, each written on its own line — which the
/// utility ends with a newline whether or not the input's last line carried one.
fn cut_lines(portion: &Portion, input: &str) -> String {
    let mut written = String::new();
    for line in lines_of(input) {
        match portion {
            Portion::Characters(ranges) => {
                for (index, character) in line.chars().enumerate() {
                    if ranges.holds(index.saturating_add(1)) {
                        written.push(character);
                    }
                }
            }
            Portion::Fields {
                ranges,
                delimiter,
            } => {
                let fields: Vec<&str> = line.split(*delimiter).collect();
                // "Lines with no delimiters are passed through untouched", which is the
                // utility's default without `-s`.
                if fields.len() == 1 {
                    written.push_str(line);
                } else {
                    let selected: Vec<&str> = fields
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| ranges.holds(index.saturating_add(1)))
                        .map(|(_, field)| *field)
                        .collect();
                    written.push_str(&selected.join(&delimiter.to_string()));
                }
            }
        }
        written.push('\n');
    }
    written
}

/// The first `lines` lines of `input`, each with the newline it carried.
fn head_of(lines: usize, input: &str) -> String {
    let mut written = String::new();
    let ends_with_newline = input.ends_with('\n');
    let held = lines_of(input);
    let kept = held.len().min(lines);
    for (index, line) in held.iter().take(lines).enumerate() {
        written.push_str(line);
        // The last line keeps a newline only if the input's own last line did.
        if index + 1 < kept || ends_with_newline {
            written.push('\n');
        }
    }
    written
}

/// The lowercase hexadecimal digest of `input`.
fn digest_of(digester: Digester, input: &str) -> String {
    match digester {
        Digester::Sha256 => {
            let mut hasher = ::sha2::Sha256::new();
            hasher.update(input.as_bytes());
            ::hex::encode(hasher.finalize())
        }
        Digester::Sha512 => {
            let mut hasher = ::sha2::Sha512::new();
            hasher.update(input.as_bytes());
            ::hex::encode(hasher.finalize())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One invocation, as a command line's words.
    fn words(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|word| (*word).to_string()).collect()
    }

    /// RULE: a filter writes what it computed over its ONE input, in the output form its own
    /// utility is specified to write.
    ///
    /// The rows differ along what the rule generalises over: which utility, which option, how
    /// the option's value is written, what the input holds and whether it came from a file or
    /// from standard input. The digests are of the input bytes, so each row's expectation is
    /// computed the same way the utility's own specification states — see the per-row note.
    #[test]
    fn a_filter_writes_what_it_computed() {
        // (case, the utility, its arguments, the label, the input, what it writes)
        /// One row: the case, the utility, its arguments, the label the input was read from,
        /// the input itself, and what the filter writes for it.
        type Row = (
            &'static str,
            &'static str,
            Vec<String>,
            &'static str,
            &'static str,
            String,
        );
        let rows: Vec<Row> = vec![
            (
                "wc -c counts the bytes of standard input",
                "wc",
                words(&["-c"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "6\n".to_string(),
            ),
            (
                "wc -c counts bytes, not characters",
                "wc",
                words(&["-c"]),
                STANDARD_INPUT_LABEL,
                "héllo",
                "6\n".to_string(),
            ),
            (
                "wc -m counts characters",
                "wc",
                words(&["-m"]),
                STANDARD_INPUT_LABEL,
                "héllo",
                "5\n".to_string(),
            ),
            (
                "wc -l counts newlines",
                "wc",
                words(&["-l"]),
                STANDARD_INPUT_LABEL,
                "one\ntwo\nthree",
                "2\n".to_string(),
            ),
            (
                "wc -w counts words",
                "wc",
                words(&["-w"]),
                STANDARD_INPUT_LABEL,
                "  one   two\tthree\n",
                "3\n".to_string(),
            ),
            (
                "wc names the file it read",
                "wc",
                words(&["-c", "notes.txt"]),
                "notes.txt",
                "abc",
                "3 notes.txt\n".to_string(),
            ),
            (
                "cut -c with a glued range",
                "cut",
                words(&["-c1-3"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "abc\n".to_string(),
            ),
            (
                "cut -c with the range as the next word",
                "cut",
                words(&["-c", "2-4"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "bcd\n".to_string(),
            ),
            (
                "cut -c with an open range and a list",
                "cut",
                words(&["-c1,4-"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "adef\n".to_string(),
            ),
            (
                "cut -c runs over every line",
                "cut",
                words(&["-c1"]),
                STANDARD_INPUT_LABEL,
                "abc\ndef\n",
                "a\nd\n".to_string(),
            ),
            (
                "cut -f with a delimiter",
                "cut",
                words(&["-d", ":", "-f2"]),
                STANDARD_INPUT_LABEL,
                "one:two:three\n",
                "two\n".to_string(),
            ),
            (
                "cut -f passes a line with no delimiter through",
                "cut",
                words(&["-d:", "-f2"]),
                STANDARD_INPUT_LABEL,
                "undelimited\n",
                "undelimited\n".to_string(),
            ),
            (
                "cut -f defaults to a tab delimiter",
                "cut",
                words(&["-f2"]),
                STANDARD_INPUT_LABEL,
                "one\ttwo\n",
                "two\n".to_string(),
            ),
            (
                "cut -f with a space delimiter written as the word after the option",
                "cut",
                words(&["-d", " ", "-f2"]),
                STANDARD_INPUT_LABEL,
                "one two\n",
                "two\n".to_string(),
            ),
            (
                "head keeps ten lines by default",
                "head",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n".to_string(),
            ),
            (
                "head -n keeps the count it names",
                "head",
                words(&["-n", "2"]),
                STANDARD_INPUT_LABEL,
                "1\n2\n3\n",
                "1\n2\n".to_string(),
            ),
            (
                "head -n keeps a last line with no newline as it was",
                "head",
                words(&["-n2"]),
                STANDARD_INPUT_LABEL,
                "1\n2",
                "1\n2".to_string(),
            ),
            (
                // The digest of no bytes at all, which is the published SHA-256 of the empty
                // input.
                "sha256sum of standard input",
                "sha256sum",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  -\n"
                    .to_string(),
            ),
            (
                // "abc", whose SHA-256 is the vector published with the algorithm.
                "sha256sum names the file it read",
                "sha256sum",
                words(&["payload.txt"]),
                "payload.txt",
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  \
                 payload.txt\n"
                    .to_string(),
            ),
            (
                // "abc", whose SHA-512 is the other vector published with the algorithm.
                "sha512sum of standard input",
                "sha512sum",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "abc",
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a8\
                 36ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f  -\n"
                    .to_string(),
            ),
            (
                // The vectors RFC 4648 section 10 publishes for the Base64 alphabet, which
                // are the encodings of these inputs.
                "base64 of standard input",
                "base64",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "foobar",
                "Zm9vYmFy\n".to_string(),
            ),
            (
                // "fo" pads to one `=`, which is what the alphabet's padding rule gives.
                "base64 of an input that pads",
                "base64",
                words(&["payload.txt"]),
                "payload.txt",
                "fo",
                "Zm8=\n".to_string(),
            ),
            (
                // "wrap encoded lines after COLS character": 60 bytes of input encode to 80
                // characters, so the default width writes 76 of them and then the rest.
                "base64 wrapped at the default width",
                "base64",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFh\nYWFh\n"
                    .to_string(),
            ),
            (
                // "-w, --wrap=COLS ... Use 0 to disable line wrapping."
                "base64 with the wrapping disabled",
                "base64",
                words(&["-w", "0"]),
                STANDARD_INPUT_LABEL,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFh\n"
                    .to_string(),
            ),
            (
                "base64 with a wrapping width written as one word",
                "base64",
                words(&["--wrap=4"]),
                STANDARD_INPUT_LABEL,
                "foobar",
                "Zm9v\nYmFy\n".to_string(),
            ),
            (
                "base64 of nothing at all",
                "base64",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "",
                String::new(),
            ),
            (
                "base64 -d of an encoding",
                "base64",
                words(&["-d"]),
                STANDARD_INPUT_LABEL,
                "Zm9vYmFy",
                "foobar".to_string(),
            ),
            (
                // The line breaks an encoding wrote are not data, and neither is the newline
                // the stage before a decode ended its output with.
                "base64 --decode of a wrapped encoding",
                "base64",
                words(&["--decode"]),
                STANDARD_INPUT_LABEL,
                "Zm9v\nYmFy\n",
                "foobar".to_string(),
            ),
            (
                "base64 -d of a file",
                "base64",
                words(&["-d", "encoded.txt"]),
                "encoded.txt",
                "RlJPTSBzY3JhdGNo\n",
                "FROM scratch".to_string(),
            ),
            (
                // "-i, --ignore-garbage: when decoding, ignore non-alphabet characters".
                "base64 -d -i of an encoding carrying garbage",
                "base64",
                words(&["-d", "-i"]),
                STANDARD_INPUT_LABEL,
                "Zm9v*Ym Fy",
                "foobar".to_string(),
            ),
        ];
        for (case, name, arguments, label, input, expected) in rows {
            let filter = Filter::read(name, &arguments)
                .unwrap_or_else(|| panic!("case {case:?}: expected {name} {arguments:?} to be a \
                                           form this module performs"));
            let written = filter.perform(label, input).unwrap_or_else(|refusal| {
                panic!("case {case:?}: expected an output, got the refusal {refusal:?}")
            });
            assert_eq!(
                written.written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: a utility, option or operand form whose output does not follow from ONE input is
    /// not performed at all.
    #[test]
    fn a_form_outside_the_catalog_is_not_performed() {
        // (case, the utility, its arguments)
        let rows: Vec<(&str, &str, Vec<String>)> = vec![
            ("wc with no count option", "wc", words(&[])),
            ("wc with two different counts", "wc", words(&["-c", "-l"])),
            ("wc with an option it has none of", "wc", words(&["-L"])),
            ("two file operands", "wc", words(&["-c", "one", "two"])),
            ("cut with no list", "cut", words(&["-d:"])),
            ("cut selecting both kinds at once", "cut", words(&["-c1", "-f1"])),
            ("cut with a list that names nothing", "cut", words(&["-c0"])),
            ("cut with a reversed range", "cut", words(&["-c4-2"])),
            ("cut with a list that is not one", "cut", words(&["-cone"])),
            ("cut with a multi-character delimiter", "cut", words(&["-d::", "-f1"])),
            ("an option whose value was never written", "cut", words(&["-c"])),
            ("head with a count that is not one", "head", words(&["-n", "two"])),
            ("head with an option it has none of", "head", words(&["-c", "2"])),
            ("base64 with an option it has none of", "base64", words(&["-x"])),
            ("base64 with a width that is not one", "base64", words(&["-w", "wide"])),
            ("a utility this module has none of", "tail", words(&["-n1"])),
        ];
        for (case, name, arguments) in rows {
            let read = Filter::read(name, &arguments);
            assert!(
                read.is_none(),
                "case {case:?}: expected {name} {arguments:?} not to be performed, got {read:?}"
            );
        }
    }

    /// RULE: an input no Base64 encoding decodes from is the utility's own failure, and an
    /// encoding ROUND-TRIPS: what an encode wrote decodes back to the very bytes it read,
    /// whatever width it was wrapped at.
    ///
    /// The refusal rows differ along how the input breaks the alphabet; the round-trip rows
    /// along the input's length (which decides the padding), the characters it carries and
    /// the wrapping width, so a line break in the middle of an encoding is covered too.
    #[test]
    fn a_base64_decode_answers_the_bytes_an_encode_read_or_fails() {
        // (case, the arguments, the input, the standard-error line it writes)
        let refused: Vec<(&str, Vec<String>, &str, &str)> = vec![
            (
                "a character outside the alphabet",
                words(&["-d"]),
                "Zm9v*YmFy",
                BASE64_INVALID_INPUT,
            ),
            (
                "an encoding one character short of a quantum",
                words(&["-d"]),
                "Zm9vYmFyZ",
                BASE64_INVALID_INPUT,
            ),
            (
                // A session's streams and files carry text, so bytes that are none are not
                // written as something else: `/w==` decodes to the byte 0xff, which is no
                // UTF-8 sequence.
                "an encoding of bytes that are not text",
                words(&["-d"]),
                "/w==",
                BASE64_UNDECODABLE_BYTES,
            ),
        ];
        for (case, arguments, input, expected) in refused {
            let filter = Filter::read("base64", &arguments)
                .unwrap_or_else(|| panic!("case {case:?}: expected a performed form"));
            let answered = filter.perform(STANDARD_INPUT_LABEL, input);
            assert_eq!(
                answered.as_ref().err().map(String::as_str),
                Some(expected),
                "case {case:?}: expected the refusal {expected:?}, got {answered:?}"
            );
        }

        // (case, the input, the wrapping width the encode is asked for)
        let round_tripped: Vec<(&str, &str, Vec<String>)> = vec![
            ("one byte, which pads twice", "a", words(&[])),
            ("two bytes, which pad once", "ab", words(&[])),
            ("three bytes, which do not pad", "abc", words(&[])),
            (
                "a whole Dockerfile, wrapped at the default width",
                "FROM scratch\nARG BODY\nENV BODY=$BODY\nEXPOSE 80\nCMD [\"serve\"]\n",
                words(&[]),
            ),
            (
                "the same text with the wrapping disabled",
                "FROM scratch\nARG BODY\nENV BODY=$BODY\nEXPOSE 80\nCMD [\"serve\"]\n",
                words(&["-w0"]),
            ),
            (
                "text wrapped at a width that splits every quantum",
                "the quick brown fox jumps over the lazy dog",
                words(&["--wrap", "5"]),
            ),
            ("text outside ASCII", "héllo wörld — ⚙", words(&[])),
        ];
        for (case, input, arguments) in round_tripped {
            let encoder = Filter::read("base64", &arguments)
                .unwrap_or_else(|| panic!("case {case:?}: expected a performed encode"));
            let encoded = encoder
                .perform(STANDARD_INPUT_LABEL, input)
                .unwrap_or_else(|refusal| panic!("case {case:?}: expected an encoding, got {refusal:?}"))
                .written;
            let decoder = Filter::read("base64", &words(&["-d"]))
                .unwrap_or_else(|| panic!("case {case:?}: expected a performed decode"));
            let decoded = decoder
                .perform(STANDARD_INPUT_LABEL, &encoded)
                .unwrap_or_else(|refusal| panic!("case {case:?}: expected a decoding, got {refusal:?}"))
                .written;
            assert_eq!(
                decoded, input,
                "case {case:?}: expected {input:?} back from {encoded:?}, got {decoded:?}"
            );
        }
    }

    /// RULE: a file operand is the input the filter reads, and `-` names standard input — so a
    /// session knows which to hand it.
    #[test]
    fn the_operand_names_where_the_input_comes_from() {
        // (case, the utility, its arguments, the file it reads)
        let rows: Vec<(&str, &str, Vec<String>, Option<&str>)> = vec![
            ("no operand at all", "sha256sum", words(&[]), None),
            (
                "the standard-input spelling",
                "sha256sum",
                words(&["-"]),
                None,
            ),
            (
                "a file operand",
                "sha256sum",
                words(&["tree/file.txt"]),
                Some("tree/file.txt"),
            ),
            (
                "a file operand after an option",
                "cut",
                words(&["-c1", "file.txt"]),
                Some("file.txt"),
            ),
            (
                "an option whose value is the next word is not an operand",
                "cut",
                words(&["-c", "1-2"]),
                None,
            ),
        ];
        for (case, name, arguments, expected) in rows {
            let filter = Filter::read(name, &arguments)
                .unwrap_or_else(|| panic!("case {case:?}: expected a performed form"));
            let file = filter.file();
            assert_eq!(
                file, expected,
                "case {case:?}: expected {expected:?}, got {file:?}"
            );
        }
    }

    /// The text a `grep` case is run over: lines that differ in case, in what they hold after
    /// the colon, and in the spacing a JSON document writes around one.
    const SELECTED_FROM: &str = "\"isMaster\": true\n\
                                 \"instanceId\": \"i-0a1b2c3d\"\n\
                                 ISMASTER keyword\n\
                                 \"isMaster\":false\n\
                                 a+b literal\n";

    /// RULE: `grep` writes the lines of its ONE input the pattern selects, in the form its
    /// options ask for, and its STATUS is the selection it made — zero when a line was
    /// selected, one when none was.
    ///
    /// The rows differ along what the rule generalises over: which option selects or writes
    /// (none, `-q`, `-c`, `-v`, `-i`, `-o`, `-n`), the flavour the pattern is read in (basic,
    /// `-E`, `-F`), where the pattern comes from (the first operand, `-e`, `--regexp=`),
    /// whether anything matched at all, and the input the lines come from.
    #[test]
    fn grep_selects_the_lines_its_pattern_matches() {
        // (case, arguments, input, what is written, whether a line was selected)
        let rows: Vec<(&str, Vec<String>, &str, &str, bool)> = vec![
            (
                // The idiom the Amazon EMR documentation reads a node's role with.
                "a basic pattern whose `*` quantifies the space before a value",
                words(&["-q", "\"isMaster\": *true"]),
                SELECTED_FROM,
                "",
                true,
            ),
            (
                "the same pattern over a document that holds the other value",
                words(&["-q", "\"isMaster\": *true"]),
                "\"isMaster\": false\n",
                "",
                false,
            ),
            (
                "a pattern selecting two lines, written as they were read",
                words(&["isMaster"]),
                SELECTED_FROM,
                "\"isMaster\": true\n\"isMaster\":false\n",
                true,
            ),
            (
                "-c counts the selected lines instead of writing them",
                words(&["-c", "isMaster"]),
                SELECTED_FROM,
                "2\n",
                true,
            ),
            (
                "-c of a pattern nothing matches counts none",
                words(&["-c", "isWorker"]),
                SELECTED_FROM,
                "0\n",
                false,
            ),
            (
                "-v selects the lines the pattern does not match",
                words(&["-v", "isMaster"]),
                "\"isMaster\": true\nkept\n",
                "kept\n",
                true,
            ),
            (
                "-i matches without regard to case",
                words(&["-i", "ismaster"]),
                SELECTED_FROM,
                "\"isMaster\": true\nISMASTER keyword\n\"isMaster\":false\n",
                true,
            ),
            (
                "-o writes only the part that matched",
                words(&["-o", "\"i[a-zA-Z]*\""]),
                "\"instanceId\": \"i-0a1b2c3d\"\n",
                "\"instanceId\"\n",
                true,
            ),
            (
                "-n prefixes each selected line with its number",
                words(&["-n", "isMaster"]),
                SELECTED_FROM,
                "1:\"isMaster\": true\n4:\"isMaster\":false\n",
                true,
            ),
            (
                // `(` and `|` are literal in a basic expression and special in an extended
                // one, which is the whole difference the flavour makes.
                "-E reads the pattern as an extended expression",
                words(&["-E", "(isMaster|instanceId)"]),
                "\"instanceId\": \"i-1\"\n",
                "\"instanceId\": \"i-1\"\n",
                true,
            ),
            (
                "a basic pattern holds `+` as a literal",
                words(&["a+b"]),
                SELECTED_FROM,
                "a+b literal\n",
                true,
            ),
            (
                "-F holds every character of the pattern as a literal",
                words(&["-F", "\"isMaster\": true"]),
                SELECTED_FROM,
                "\"isMaster\": true\n",
                true,
            ),
            (
                "-F of a pattern whose metacharacters match nothing literally",
                words(&["-F", "is.*Master"]),
                SELECTED_FROM,
                "",
                false,
            ),
            (
                "the pattern named by -e rather than by the first operand",
                words(&["-e", "instanceId"]),
                SELECTED_FROM,
                "\"instanceId\": \"i-0a1b2c3d\"\n",
                true,
            ),
            (
                "the pattern named by --regexp=",
                words(&["--regexp=instanceId"]),
                SELECTED_FROM,
                "\"instanceId\": \"i-0a1b2c3d\"\n",
                true,
            ),
            (
                "an empty input selects nothing",
                words(&["isMaster"]),
                "",
                "",
                false,
            ),
            (
                "a last line with no newline is still a line",
                words(&["isMaster"]),
                "\"isMaster\": true",
                "\"isMaster\": true\n",
                true,
            ),
        ];
        for (case, arguments, input, expected, selected) in rows {
            let filter = Filter::read("grep", &arguments).unwrap_or_else(|| {
                panic!("case {case:?}: expected grep {arguments:?} to be a performed form")
            });
            let performed = filter
                .perform(STANDARD_INPUT_LABEL, input)
                .unwrap_or_else(|refusal| {
                    panic!("case {case:?}: expected an output, got the refusal {refusal:?}")
                });
            assert_eq!(
                performed.written, expected,
                "case {case:?}: expected {expected:?} written, got {performed:?}"
            );
            assert_eq!(
                performed.selected,
                Some(selected),
                "case {case:?}: expected the selection {selected:?}, got {performed:?}"
            );
        }
    }

    /// RULE's fail-closed half: a `grep` form whose output or whose MEANING does not follow
    /// from one input and one engine-readable pattern is not performed at all.
    #[test]
    fn a_grep_form_outside_the_catalog_is_not_performed() {
        // (case, the arguments)
        let rows: Vec<(&str, Vec<String>)> = vec![
            ("no pattern at all", words(&[])),
            (
                "a basic pattern carrying an escape the flavours disagree on",
                words(&[r"\(isMaster\)"]),
            ),
            ("-o and -v together", words(&["-o", "-v", "isMaster"])),
            ("two file operands", words(&["isMaster", "one", "two"])),
            ("an option it has none of", words(&["-r", "isMaster"])),
            ("the lines around a match", words(&["-A2", "isMaster"])),
            ("the files rather than the lines", words(&["-l", "isMaster"])),
            ("a cluster carrying an option it has none of", words(&["-qr", "isMaster"])),
            ("two patterns", words(&["-e", "one", "-e", "two"])),
            ("an extended pattern the engine cannot read", words(&["-E", "("])),
            ("a pattern option whose value was never written", words(&["-e"])),
        ];
        for (case, arguments) in rows {
            let read = Filter::read("grep", &arguments);
            assert!(
                read.is_none(),
                "case {case:?}: expected grep {arguments:?} not to be performed, got {read:?}"
            );
        }
    }
}
