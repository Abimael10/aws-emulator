// SPDX-License-Identifier: CC0-1.0

//! The IDENTITY of the interpreter the runtime a function is configured with names — which
//! implementation of Python it is and which RELEASE of it — as the interpreter one invocation
//! runs in reports it.
//!
//! A managed `python3.x` runtime IS a release of the language: the identifier names the
//! interpreter's own minor version, so a handler of a function created with one identifier
//! reads that version of Python and a handler of a function created with another reads
//! another's. The identifier is the only statement of a version there is, which is why it is
//! what a run reports — the same reading the node engine reports its own release from
//! ([`super::node`]'s `node_version`), and the same reading the machine a run reports comes
//! from the function's `Architectures` rather than from the machine this process happens to
//! be on ([`super::host_posix::install_environment_identity`]).
//!
//! WHICH runtime a run is of is read where the rest of the runtime's own identity is read
//! from: the invocation's environment, whose `AWS_EXECUTION_ENV`
//! ([`super::EXECUTION_ENV_VARIABLE`]) the service exports from the function's configuration
//! and a function may not set itself. It is the same reading the runtime's library directory
//! is derived from ([`super::bootstrap`]'s `_runtime_library`). An embedder that hands this
//! runtime no environment at all — a script run of another service, which is nobody's Lambda
//! invocation — names no runtime, so nothing is installed for it and the interpreter reports
//! its own build.
//!
//! WHICH IMPLEMENTATION of the language a managed runtime is is the same kind of statement: a
//! `python3.x` runtime is a CPython release ([`IMPLEMENTATION_NAME`]), so that is the
//! implementation a handler of such a function observes — through `sys.implementation`, and
//! through the version TEXT the standard library's `platform` module derives the running
//! implementation from. WHICH interpreter this emulator embeds to run the handler is this
//! emulator's own affair and no statement of the function's runtime, so no reading of the
//! runtime's identity reports it: where the interpreter states its own implementation, the
//! implementation the runtime IS is stated instead.
//!
//! The installation is the HOST's, before a line of guest code is read, and it is READ BACK
//! afterwards: an installation that did not take would leave a run reporting a release or an
//! implementation its function was not created with, and no later reading would say so.

use std::collections::BTreeMap;

use rustpython_vm::{AsObject, PyObjectRef, PyResult, VirtualMachine};

/// The patch level a run reports for the release its runtime names.
///
/// A `Runtime` identifier names a minor version and nothing further (`python3.12` is the 3.12
/// line), so the line's own first release is what is reported — the reading the node engine
/// reports its release under for the same reason ([`super::node`]'s `node_version`).
const MICRO: u8 = 0;

/// The release level of a managed runtime's interpreter: a published release, which is what
/// `final` is in a Python version.
const RELEASE_LEVEL: &str = "final";

/// The serial a `final` release carries, which is zero for every one of them.
const RELEASE_SERIAL: u8 = 0;

/// How `final` is encoded in the single number a Python version is packed into
/// (`sys.hexversion`, whose release-level nibble is `0xF` for a final release).
const FINAL_LEVEL: u32 = 0xF;

/// The MAJOR version of the language this engine runs, which is the one a release it reports
/// can be a release of.
///
/// The interpreter embedded here is a Python 3 one, so an identifier naming another major
/// version names a language this engine is not and no release is installed for it: the run
/// then reports the interpreter's own build rather than an identity with nothing behind it.
/// Every runtime of another major version is one Lambda no longer supports
/// ([`crate::state::DEPRECATED_RUNTIMES`] holds `python2.7`), and what the supported
/// identifiers state is the MINOR version of this same language, which is what is installed.
const LANGUAGE_MAJOR: u8 = 3;

/// The fields of the version a Python program reads off the interpreter, in the order that
/// version carries them (`sys.version_info`: `major`, `minor`, `micro`, `releaselevel`,
/// `serial`).
const FIELD_NAMES: &[&str] = &["major", "minor", "micro", "releaselevel", "serial"];

/// The IMPLEMENTATION of Python a managed `python3.x` runtime is, as a Python program reads an
/// implementation's own name (`sys.implementation.name`, which is lower case).
///
/// A managed runtime is one implementation of the language, and the `python3.x` runtimes are
/// CPython releases — so that is the implementation a handler of such a function observes,
/// exactly as the RELEASE it observes is the one its `Runtime` identifier names and the machine
/// it observes is the one its `Architectures` names
/// ([`super::host_posix::install_environment_identity`]).
const IMPLEMENTATION_NAME: &str = "cpython";

/// The same implementation as the version TEXT names one.
///
/// The text is the one reading the standard library's `platform` module derives the running
/// implementation from (`platform.python_implementation()`), and an implementation is named
/// there the way it is written rather than the way it is keyed.
const IMPLEMENTATION_TITLE: &str = "CPython";

/// Where an implementation states the machine it is built for, as `sys.implementation` carries
/// that statement on the systems that have one.
const MULTIARCH: &str = "_multiarch";

/// What that statement separates the machine from the rest of itself by.
const MULTIARCH_SEPARATOR: char = '-';

/// The release `runtime` names: the major and minor version of the interpreter a function
/// configured with that identifier runs on, or `None` for an identifier that names no release
/// of this language at all ([`LANGUAGE_MAJOR`]).
#[must_use]
pub(super) fn named_by(runtime: &str) -> Option<(u8, u8)> {
    let named = runtime.strip_prefix(super::PYTHON_RUNTIME_PREFIX)?;
    let (major, minor) = named.split_once('.')?;
    let major = leading_number(major)?;
    if major != LANGUAGE_MAJOR {
        return None;
    }
    Some((major, leading_number(minor)?))
}

/// The number `part` begins with, so a suffix an identifier carries beside the number names
/// the same release as the number alone, and `None` where it begins with no digit.
///
/// One BYTE each, which is what a component of a Python version is — the packed version holds
/// each in one ([`hexversion`]) — so a part naming a number past that names no release of this
/// language, exactly as a part naming no number at all does.
fn leading_number(part: &str) -> Option<u8> {
    let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The runtime identifier `environment` names, out of the variable a managed runtime exports
/// it as.
#[must_use]
fn runtime_of(environment: &BTreeMap<String, String>) -> Option<&str> {
    environment
        .get(super::EXECUTION_ENV_VARIABLE)?
        .strip_prefix(super::EXECUTION_ENV_PREFIX)
}

/// Install the release the run's own runtime names on `vm`, where a Python program reads the
/// version of the interpreter it is running on.
///
/// Three readings carry the release, and all three are installed together so no two of them
/// disagree: the version `sys.version_info` reports field by field, the version TEXT
/// `sys.version` carries (which is what the standard library's `platform` module reads the
/// running version out of), and the single number `sys.hexversion` packs it into. Only the
/// version and the IMPLEMENTATION are restated in the text: what the interpreter says about
/// its own build beside those stays as it is, because that is this build's and not the
/// runtime's.
///
/// The implementation the runtime is goes in beside them ([`install_implementation`]), read
/// from the same runtime identifier, so the implementation `sys.implementation` reports and the
/// one the text names are one statement.
pub(super) fn install(
    vm: &VirtualMachine,
    environment: &BTreeMap<String, String>,
    architecture: &str,
) -> PyResult<()> {
    let Some((major, minor)) = runtime_of(environment).and_then(named_by) else {
        return Ok(());
    };
    let system = vm.sys_module.as_object();
    let release = release_object(vm, major, minor)?;
    system.set_attr("version_info", release.clone(), vm)?;
    // Read back by the HOST: the run reports the release its function was created with, or it
    // is not served at all.
    let installed = system.get_attr("version_info", vm)?;
    if read_release(vm, &installed) != Some((i64::from(major), i64::from(minor))) {
        return Err(vm.new_runtime_error(format!(
            "the interpreter of this execution environment does not report the {major}.{minor} \
             release its runtime names, so no function is run in it"
        )));
    }
    // WHAT the interpreter states its own implementation to be, read before anything restates
    // it: it is the name the version text identifies the engine by, and that text is the one
    // reading `platform` derives the running implementation from. Read with the rest: an
    // interpreter whose own implementation cannot be read is one whose statement of itself
    // cannot be restated either, so no function is run in it rather than one being run in an
    // environment reporting an implementation its runtime is not.
    let identity = system.get_attr("implementation", vm)?;
    let engine: String = identity.get_attr("name", vm)?.try_into_value(vm)?;
    let built: String = system.get_attr("version", vm)?.try_into_value(vm)?;
    system.set_attr(
        "version",
        vm.ctx.new_str(version_text(&built, &engine, major, minor)),
        vm,
    )?;
    // Read back by the HOST, as the release is: the text a run reports names the implementation
    // the runtime is and no other, because that text is where `platform` reads the running
    // implementation from.
    let stated: String = system.get_attr("version", vm)?.try_into_value(vm)?;
    if names_implementation(&stated, &engine) && !engine.eq_ignore_ascii_case(IMPLEMENTATION_TITLE)
    {
        return Err(vm.new_runtime_error(format!(
            "the version this execution environment reports still names the {engine} \
             implementation rather than the {IMPLEMENTATION_NAME} its runtime is, so no function \
             is run in it"
        )));
    }
    system.set_attr("hexversion", vm.ctx.new_int(hexversion(major, minor)), vm)?;
    install_implementation(vm, &identity, &release, (major, minor), architecture)
}

/// Install the IMPLEMENTATION this execution environment runs, where a Python program reads the
/// implementation it is running on (`sys.implementation`): the implementation a managed runtime
/// is ([`IMPLEMENTATION_NAME`]), the release that runtime names — the same object
/// `sys.version_info` is, so the two readings cannot disagree — the single number that release
/// packs into, the cache tag the implementation and the release make together, and the machine
/// the function's own architecture names.
///
/// The object the identity is stated on is put back on `sys` and READ BACK from there, for the
/// same reason the release is: an installation that did not take would leave a run reporting an
/// implementation its function's runtime is not, and no later reading would say so.
fn install_implementation(
    vm: &VirtualMachine,
    identity: &PyObjectRef,
    release: &PyObjectRef,
    (major, minor): (u8, u8),
    architecture: &str,
) -> PyResult<()> {
    identity.set_attr("name", vm.ctx.new_str(IMPLEMENTATION_NAME), vm)?;
    identity.set_attr("version", release.clone(), vm)?;
    identity.set_attr("hexversion", vm.ctx.new_int(hexversion(major, minor)), vm)?;
    identity.set_attr("cache_tag", vm.ctx.new_str(cache_tag(major, minor)), vm)?;
    // The machine the identity names, where it names one at all: the function's own
    // architecture, which is the machine every other reading of this environment reports
    // ([`super::host_posix::install_environment_identity`]) rather than the one this process
    // happens to be on.
    if let Some(held) = identity
        .get_attr(MULTIARCH, vm)
        .ok()
        .and_then(|held| held.try_into_value::<String>(vm).ok())
    {
        identity.set_attr(
            MULTIARCH,
            vm.ctx.new_str(multiarch_of(&held, architecture)),
            vm,
        )?;
    }
    let system = vm.sys_module.as_object();
    system.set_attr("implementation", identity.clone(), vm)?;
    let installed: String = system
        .get_attr("implementation", vm)?
        .get_attr("name", vm)?
        .try_into_value(vm)?;
    if installed != IMPLEMENTATION_NAME {
        return Err(vm.new_runtime_error(format!(
            "the interpreter of this execution environment does not report the \
             {IMPLEMENTATION_NAME} implementation its runtime is, so no function is run in it"
        )));
    }
    Ok(())
}

/// What an implementation and a release are keyed together by, as `sys.implementation.cache_tag`
/// carries that key: the implementation's own name and the release's major and minor version
/// written after it.
fn cache_tag(major: u8, minor: u8) -> String {
    format!("{IMPLEMENTATION_NAME}-{major}{minor}")
}

/// The machine statement of an implementation running a function on `architecture`: the machine
/// that architecture names ([`super::host_posix::machine_of`]), followed by whatever `held`
/// states about the environment the implementation is built for.
fn multiarch_of(held: &str, architecture: &str) -> String {
    let machine = super::host_posix::machine_of(architecture);
    match held.split_once(MULTIARCH_SEPARATOR) {
        Some((_, rest)) => format!("{machine}{MULTIARCH_SEPARATOR}{rest}"),
        None => machine.to_string(),
    }
}

/// The version a Python program reads field by field, as the object `sys.version_info` is.
///
/// Built as the interpreter's OWN version type where that type can be built from its fields,
/// so what a handler reads is the object the interpreter would have had; where it cannot, the
/// object is the tuple with the same named fields ([`FIELD_NAMES`]), which is what a Python
/// program reads a version through — indexed, sliced, compared against a tuple and asked for
/// a field by name.
fn release_object(vm: &VirtualMachine, major: u8, minor: u8) -> PyResult<PyObjectRef> {
    let fields: Vec<PyObjectRef> = vec![
        vm.ctx.new_int(major).into(),
        vm.ctx.new_int(minor).into(),
        vm.ctx.new_int(MICRO).into(),
        vm.ctx.new_str(RELEASE_LEVEL).into(),
        vm.ctx.new_int(RELEASE_SERIAL).into(),
    ];
    let held: PyObjectRef = vm.ctx.new_tuple(fields.clone()).into();
    let existing = vm.sys_module.as_object().get_attr("version_info", vm)?;
    let own: PyObjectRef = existing.class().to_owned().into();
    if let Ok(built) = own.call((held,), vm) {
        if read_release(vm, &built) == Some((i64::from(major), i64::from(minor))) {
            return Ok(built);
        }
    }
    named_fields(vm)?.call(fields, vm)
}

/// The TYPE of a version with the fields above, defined in a scope of its own.
fn named_fields(vm: &VirtualMachine) -> PyResult<PyObjectRef> {
    let scope = vm.new_scope_with_builtins();
    let source = format!(
        "import collections\n\
         version_info = collections.namedtuple(\"version_info\", {FIELD_NAMES:?})\n"
    );
    let code = vm
        .compile(
            &source,
            ::rustpython_vm::compiler::Mode::Exec,
            "<lambda-release>".to_owned(),
        )
        .map_err(|error| {
            vm.new_runtime_error(format!("the version source does not compile: {error}"))
        })?;
    vm.run_code_obj(code, scope.clone())?;
    scope.globals.get_item("version_info", vm)
}

/// The major and minor version `held` reports, read both by position and by name — the two
/// ways a Python program reads a version — and `None` unless both readings agree.
fn read_release(vm: &VirtualMachine, held: &PyObjectRef) -> Option<(i64, i64)> {
    let at = |position: u32| -> Option<i64> {
        let key: PyObjectRef = vm.ctx.new_int(position).into();
        held.get_item(&*key, vm).ok()?.try_into_value(vm).ok()
    };
    let named =
        |name: &'static str| -> Option<i64> { held.get_attr(name, vm).ok()?.try_into_value(vm).ok() };
    let read = (at(0)?, at(1)?);
    if (named(FIELD_NAMES[0])?, named(FIELD_NAMES[1])?) != read {
        return None;
    }
    Some(read)
}

/// The version TEXT a run carries: the release its runtime names, the implementation that
/// runtime is, and whatever the interpreter states about its own build beside the two.
///
/// The text is what `platform.python_version()` reads the running version out of and what
/// `platform.python_implementation()` reads the running implementation from, so both of those
/// are the runtime's: the version it begins with is the release, and a statement of `engine` —
/// the implementation the interpreter states ITSELF to be — is restated as the implementation
/// the runtime is ([`restated`]). Everything else is this build's own and is left exactly as it
/// is.
fn version_text(built: &str, engine: &str, major: u8, minor: u8) -> String {
    let release = format!("{major}.{minor}.{MICRO}");
    let stated = match built.split_once(char::is_whitespace) {
        Some((_, rest)) => format!("{release} {rest}"),
        None => release.clone(),
    };
    restated(&stated, engine, &release)
}

/// `text` with every statement of the implementation named `engine` restated as the
/// implementation and release this runtime is: the name itself, and the version written right
/// after it, which is that implementation's own.
///
/// The name is matched however it is written, because an implementation names itself one way in
/// the text it builds and another in the key it is read by. A text that states no
/// implementation, and an interpreter that names none for itself, are left as they are.
fn restated(text: &str, engine: &str, release: &str) -> String {
    if engine.is_empty() {
        return text.to_string();
    }
    // Lowered ONCE, for the whole scan: lowering an ASCII letter leaves every byte position
    // where it was, so a position found in this copy is a position in `text` itself.
    let lowered = text.to_ascii_lowercase();
    let wanted = engine.to_ascii_lowercase();
    let mut rendered = String::new();
    let mut at = 0_usize;
    while let Some(found) = lowered[at..].find(wanted.as_str()) {
        let stands = at + found;
        rendered.push_str(&text[at..stands]);
        rendered.push_str(IMPLEMENTATION_TITLE);
        at = stands + wanted.len();
        // The version written right after the name is the version OF that implementation, so
        // the release this runtime names takes its place; anything else after the name is a
        // statement of something other than the implementation and stays.
        let rest = &text[at..];
        let spaces = rest.len() - rest.trim_start_matches(' ').len();
        let stated: String = rest[spaces..].chars().take_while(in_version).collect();
        if stated.starts_with(|character: char| character.is_ascii_digit()) {
            rendered.push_str(&rest[..spaces]);
            rendered.push_str(release);
            at += spaces + stated.len();
        }
    }
    rendered.push_str(&text[at..]);
    rendered
}

/// Whether `character` can stand in the VERSION a text states of an implementation, which is
/// read the way the standard library's `platform` module reads one: word characters, the dots
/// between a version's own parts, and the plus a local version carries.
fn in_version(character: &char) -> bool {
    character.is_alphanumeric() || matches!(character, '.' | '+' | '_')
}

/// Whether `text` states the implementation `named`, however either of them is written: the
/// reading `platform` makes of a version text to decide which implementation is running, and
/// the one the host reads the installed text back by.
fn names_implementation(text: &str, named: &str) -> bool {
    !named.is_empty() && text.to_ascii_lowercase().contains(&named.to_ascii_lowercase())
}

/// The release packed into the single number `sys.hexversion` carries: one byte each for the
/// major, minor and patch version, then the release level and its serial.
fn hexversion(major: u8, minor: u8) -> u32 {
    (u32::from(major) << 24)
        | (u32::from(minor) << 16)
        | (u32::from(MICRO) << 8)
        | (FINAL_LEVEL << 4)
        | u32::from(RELEASE_SERIAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Runtime` identifier names the release of the interpreter a function configured with
    /// it runs on, and an identifier that names no release of this language at all names
    /// none.
    ///
    /// Source of the identifiers: the model's own `Runtime` enum
    /// (`smithy_query(query="shape", shape="Runtime")`, whose values are
    /// `crate::ops::RUNTIME_VALUES`) — every `python` member of it, including the ones no
    /// check of this issue cited, plus the members of other languages and the identifiers
    /// that carry no version.
    #[test]
    fn a_runtime_identifier_names_the_release_its_interpreter_is() {
        let cases: &[(&str, Option<(u8, u8)>)] = &[
            ("python3.6", Some((3, 6))),
            ("python3.7", Some((3, 7))),
            ("python3.8", Some((3, 8))),
            ("python3.9", Some((3, 9))),
            ("python3.10", Some((3, 10))),
            ("python3.11", Some((3, 11))),
            ("python3.12", Some((3, 12))),
            ("python3.13", Some((3, 13))),
            ("python3.14", Some((3, 14))),
            ("python3.15", Some((3, 15))),
            // An identifier that states no minor version states no release.
            ("python3", None),
            ("python", None),
            // Nor does one whose version is no version of this language: a component of a
            // Python version is one byte.
            ("python3.300", None),
            ("python3.x", None),
            // Nor one of another major version, which is another language than the one this
            // engine runs ([`LANGUAGE_MAJOR`]).
            ("python2.7", None),
            // Another language's identifier is no release of this one.
            ("nodejs22.x", None),
            ("java21", None),
            ("ruby3.4", None),
            ("provided.al2023", None),
            ("", None),
        ];
        for (runtime, expected) in cases {
            let actual = named_by(runtime);
            assert_eq!(
                actual, *expected,
                "case {runtime:?}: expected the release {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every `python` member of that same enum that names a release of the language this
    /// engine runs ([`LANGUAGE_MAJOR`]) is one this emulator runs with the Python interpreter
    /// and one whose release is read off it, so no supported identifier is run without a
    /// release to report.
    #[test]
    fn every_python_runtime_of_the_model_names_a_release() {
        let line = format!("{}{LANGUAGE_MAJOR}.", super::super::PYTHON_RUNTIME_PREFIX);
        for runtime in crate::ops::RUNTIME_VALUES
            .iter()
            .filter(|runtime| runtime.starts_with(line.as_str()))
        {
            let actual = named_by(runtime);
            assert!(
                actual.is_some(),
                "case {runtime:?}: a runtime run by this interpreter names the release it is, \
                 got {actual:?}"
            );
            assert_eq!(
                super::super::engine_for(Some(runtime)),
                Some(super::super::Engine::Python),
                "case {runtime:?}: every identifier a release is read off is one the Python \
                 interpreter runs"
            );
        }
    }

    /// The version TEXT states the release the runtime names and keeps what the interpreter
    /// states about its own build, whatever shape that statement has.
    #[test]
    fn the_version_text_restates_the_release_and_nothing_else() {
        // The text the interpreter carries, the implementation it states itself to be, the
        // release the runtime names, and the text a run of it reports. None of these texts
        // names the interpreter's own implementation, so nothing but the version is restated.
        let cases: &[(&str, &str, (u8, u8), &str)] = &[
            (
                "3.14.0 (heads/main:0000000, Jan 1 2026, 00:00:00) [rustc 1.90.0]",
                "someengine",
                (3, 11),
                "3.11.0 (heads/main:0000000, Jan 1 2026, 00:00:00) [rustc 1.90.0]",
            ),
            (
                "3.14.0 (default) [cc]",
                "someengine",
                (3, 12),
                "3.12.0 (default) [cc]",
            ),
            // A statement of nothing but a version is restated whole.
            ("3.14.0", "someengine", (3, 13), "3.13.0"),
            // And an interpreter that states no version at all is given the release's own.
            ("", "someengine", (3, 14), "3.14.0"),
            // An interpreter that names no implementation for itself has none restated.
            (
                "3.14.0 (main) [cc 1.0]",
                "",
                (3, 12),
                "3.12.0 (main) [cc 1.0]",
            ),
        ];
        for (built, engine, (major, minor), expected) in cases {
            let actual = version_text(built, engine, *major, *minor);
            assert_eq!(
                &actual, expected,
                "case {built:?}: expected the text {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the version text names the implementation the function's runtime IS, so the
    /// implementation the interpreter states about itself — and the version it states of that
    /// implementation — is never what a run reports.
    ///
    /// The rows differ along how an interpreter states itself in that text: where in the text
    /// it stands, how its name is cased, whether a version of its own follows the name, and how
    /// many times it is stated.
    #[test]
    fn the_version_text_names_the_implementation_the_runtime_is() {
        // The text the interpreter carries, the name it keys its own implementation by, the
        // release the runtime names, and the text a run of it reports.
        let cases: &[(&str, &str, (u8, u8), &str)] = &[
            // The statement an embedded interpreter makes in the field the text keeps for the
            // compiler: its own name, its own version, then the compiler itself.
            (
                "3.14.0 (heads/main:0000000, Jan 1 2026, 00:00:00)\n\
                 [RustPython 0.6.0 with rustc 1.90.0 (0000000 2026-01-01)]",
                "rustpython",
                (3, 12),
                "3.12.0 (heads/main:0000000, Jan 1 2026, 00:00:00)\n\
                 [CPython 3.12.0 with rustc 1.90.0 (0000000 2026-01-01)]",
            ),
            // The same statement in the build field instead, and twice over.
            (
                "3.14.0 (RustPython 0.6.0, Jan 1 2026, 00:00:00) [RustPython 0.6.0]",
                "rustpython",
                (3, 13),
                "3.13.0 (CPython 3.13.0, Jan 1 2026, 00:00:00) [CPython 3.13.0]",
            ),
            // A name stated with no version of its own after it: only the name is restated.
            (
                "3.14.0 (main) [rustpython]",
                "RustPython",
                (3, 9),
                "3.9.0 (main) [CPython]",
            ),
            // An interpreter whose own name is the implementation's: the release it states of
            // it is still the runtime's.
            (
                "3.14.0 (main) [CPython 3.14.0 with cc]",
                "cpython",
                (3, 11),
                "3.11.0 (main) [CPython 3.11.0 with cc]",
            ),
        ];
        for (built, engine, (major, minor), expected) in cases {
            let actual = version_text(built, engine, *major, *minor);
            assert_eq!(
                &actual, expected,
                "case {built:?}: expected the text {expected:?}, got {actual:?}"
            );
            assert!(
                !names_implementation(&actual, engine)
                    || engine.eq_ignore_ascii_case(IMPLEMENTATION_TITLE),
                "case {built:?}: the text names no implementation but the runtime's, got \
                 {actual:?}"
            );
        }
    }

    /// RULE: the implementation and the release are keyed together the way a Python program
    /// reads that key, for every release a `python3.x` runtime names.
    ///
    /// Source of the form: `sys.implementation.cache_tag`, which CPython writes as its own
    /// implementation name and the major and minor version after it.
    #[test]
    fn the_cache_tag_keys_the_implementation_and_the_release() {
        let cases: &[((u8, u8), &str)] = &[
            ((3, 9), "cpython-39"),
            ((3, 11), "cpython-311"),
            ((3, 12), "cpython-312"),
            ((3, 13), "cpython-313"),
            ((3, 15), "cpython-315"),
        ];
        for ((major, minor), expected) in cases {
            let actual = cache_tag(*major, *minor);
            assert_eq!(
                &actual, expected,
                "case {major}.{minor}: expected the tag {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the machine the implementation states it is built for is the one the function's
    /// architecture names, and never the machine this process happens to be on.
    ///
    /// Source of the architectures: the model's own `Architecture` enum
    /// (`smithy_query(query="shape", shape="Architecture")`), both of whose values are read
    /// here, each against a statement naming the other.
    #[test]
    fn the_machine_the_implementation_names_is_the_function_s() {
        // What the interpreter's own build states, the architecture the function carries, and
        // what a run of it reports.
        let cases: &[(&str, &str, &str)] = &[
            ("aarch64-linux-gnu", "x86_64", "x86_64-linux-gnu"),
            ("x86_64-linux-gnu", "arm64", "aarch64-linux-gnu"),
            ("x86_64-linux-gnu", "x86_64", "x86_64-linux-gnu"),
            ("aarch64-linux-gnu", "arm64", "aarch64-linux-gnu"),
            // A statement of nothing but a machine carries nothing else to keep.
            ("aarch64", "x86_64", "x86_64"),
            ("", "arm64", "aarch64"),
        ];
        for (held, architecture, expected) in cases {
            let actual = multiarch_of(held, architecture);
            assert_eq!(
                &actual, expected,
                "case {held:?} on {architecture}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The packed version carries the same release, byte for byte, as the fields do.
    ///
    /// The expected numbers are read the way a Python program reads that value — the major
    /// version in the top byte, the minor in the next, then the patch and the final release's
    /// own nibble.
    #[test]
    fn the_packed_version_carries_the_same_release() {
        let cases: &[((u8, u8), u32)] = &[
            ((3, 11), 0x030B_00F0),
            ((3, 12), 0x030C_00F0),
            ((3, 13), 0x030D_00F0),
            ((3, 14), 0x030E_00F0),
            ((3, 9), 0x0309_00F0),
        ];
        for ((major, minor), expected) in cases {
            let actual = hexversion(*major, *minor);
            assert_eq!(
                actual, *expected,
                "case {major}.{minor}: expected {expected:#010x}, got {actual:#010x}"
            );
        }
    }

    /// The runtime a run is of is the one its own environment names, under the variable the
    /// service exports it as.
    #[test]
    fn the_runtime_of_a_run_is_the_one_its_environment_names() {
        // The variable's value, and the identifier a run reads out of it.
        let cases: &[(Option<&str>, Option<&str>)] = &[
            (Some("AWS_Lambda_python3.11"), Some("python3.11")),
            (Some("AWS_Lambda_python3.14"), Some("python3.14")),
            (Some("AWS_Lambda_nodejs22.x"), Some("nodejs22.x")),
            // A value that is not the runtime's own statement names no runtime.
            (Some("python3.11"), None),
            // And an environment that names none at all — an embedder's run — carries none.
            (None, None),
        ];
        for (held, expected) in cases {
            let mut environment = BTreeMap::new();
            if let Some(held) = held {
                environment.insert(
                    super::super::EXECUTION_ENV_VARIABLE.to_string(),
                    (*held).to_string(),
                );
            }
            let actual = runtime_of(&environment);
            assert_eq!(
                actual, *expected,
                "case {held:?}: expected the runtime {expected:?}, got {actual:?}"
            );
        }
    }
}
