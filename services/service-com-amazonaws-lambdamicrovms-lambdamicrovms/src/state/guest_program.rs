// SPDX-License-Identifier: CC0-1.0

//! The GUEST a MicroVM runs: the program held in the image version's code artifact.
//!
//! `CodeArtifact$uri` is "The URI of the code artifact in Amazon S3", and the object at
//! that address is what a MicroVM's guest IS: the client's own code, which a MicroVM is
//! run to serve traffic with. The build resolves that object against the emulator's own
//! object store ([`super::code_artifact`]); this module reads what it fetched and keeps
//! the modules the guest is made of, so the endpoint can RUN them
//! ([`super::guest_runtime`]) instead of answering in the guest's name.
//!
//! # What is read
//!
//! * a `.zip` artifact — the archive form a build context is uploaded as — has its
//!   entries read ENTIRELY IN MEMORY: no entry is written to a host path and nothing
//!   outside this process is opened for it;
//! * an artifact that is not an archive is the guest's single module, which is the other
//!   form a one-file program is staged as.
//!
//! Two things are kept of what was read, because a container image holds both:
//!
//! * the MODULES the embedded engines can COMPILE — JavaScript source
//!   ([`JAVASCRIPT_SUFFIXES`]), Python source ([`PYTHON_SUFFIX`]) and the JSON a program
//!   reads beside it — plus whatever the image's own build steps left behind
//!   ([`super::image_build`]), which is where a context that materialises its guest with
//!   `RUN` steps declares it. A compiled artefact, a native library or a base-image layer
//!   in the artifact is NOT loaded — nothing here loads a host library — so a guest that
//!   needs one holds no module here and nothing is reported as having run;
//! * the FILES the image holds for its guest ([`GuestProgram::files`]), whether or not any
//!   engine can compile them: the build context, under the directory the command runs from
//!   (which is where a container's context is copied), and what the build's own `RUN` steps
//!   wrote, at the absolute path each wrote it. Content a guest SERVES is not code, so a
//!   `.txt` or an `.html` a step materialised is part of the image exactly as its code is
//!   ([`super::guest_machine`] seeds both).
//!
//! # What starts the guest
//!
//! Which program STARTS the guest is the image's own declaration — the command the
//! container runs — read in the order a container image declares it:
//!
//! 1. the `CMD` or `ENTRYPOINT` of the image's `Dockerfile`, in either of two forms:
//!    * an invocation of an interpreter's own HTTP FILE-SERVER module (`python3 -m
//!      http.server [port] [--directory dir]`), which names no file of the artifact at
//!      all: the guest is then that server, serving the directory the command runs in on
//!      the port the invocation names, read through the one reading of that invocation
//!      this workspace has (`::service_common::static_web_servers::served_module`);
//!    * the source file the command names, whichever interpreter it starts;
//! 2. the `main` of a `package.json` beside it, then `index.js` — Node's own module
//!    resolution ("If the main field is absent … Node.js will attempt to load
//!    `index.js`");
//! 3. the single module of a one-module artifact, whichever engine loads it — which engine
//!    that is, this module reads off the module's own suffix.
//!
//! An artifact declaring none of those has nothing that starts a guest, so it holds no
//! guest program: what starts the guest is not guessed from the content of its code.
//!
//! # Bounds
//!
//! Every bound is on what THIS PROCESS holds rather than on what the archive declares,
//! because the declared sizes are the caller's: [`MAX_ARTIFACT_BYTES`] of fetched object,
//! [`MAX_ENTRIES`] entries read, [`MAX_PROGRAM_BYTES`] of inflated source across the
//! archive and [`MAX_MODULE_BYTES`] of any one module, each counted as the bytes are
//! produced.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};

use ::service_common::posix_shell_session::{
    command_base_name, Bounds, EnvironmentPrograms, ProgramFiles, ProgramInvocation,
    ProgramOutcome, ProgramStreams, SessionConfig, ShellSession, Statuses, NESTED_SHELL_NAMES,
};
use ::service_common::static_web_servers::{
    served_module, ServedModule, HTTP_SERVER_MODULE, INTERPRETERS,
};

/// How many bytes of a fetched code artifact are read into this process.
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

/// How many entries of one archive are read.
pub const MAX_ENTRIES: usize = 2_048;

/// How many INFLATED bytes of source one artifact may yield.
pub const MAX_PROGRAM_BYTES: usize = 8 * 1024 * 1024;

/// How many INFLATED bytes ONE module may hold.
pub const MAX_MODULE_BYTES: usize = 2 * 1024 * 1024;

/// The suffixes of an entry the embedded JavaScript engine can load: the module forms
/// Node resolves (`.js`, and the explicit `.mjs`/`.cjs`) and the JSON a program reads
/// beside its code.
const JAVASCRIPT_SUFFIXES: [&str; 4] = [".js", ".mjs", ".cjs", ".json"];

/// The suffix of an entry the embedded PYTHON engine can load.
const PYTHON_SUFFIX: &str = ".py";

/// The module Node loads when a package declares no `main`: "If the main field is absent
/// … Node.js will attempt to load `index.js`".
const DEFAULT_ENTRY: &str = "index.js";

/// The file a container image's build context declares its command in.
const CONTAINER_SPEC: &str = "Dockerfile";

/// The instructions that declare the command the container RUNS, which is what starts its
/// guest: a later one replaces an earlier.
const COMMAND_INSTRUCTIONS: [&str; 2] = ["CMD", "ENTRYPOINT"];

/// The option a shell is given the PROGRAM to run with (`/bin/sh -c PROGRAM`), which is how
/// a container's command is written when it is a shell program rather than one vector.
const SHELL_COMMAND_OPTION: &str = "-c";

/// How many commands the reading of ONE image's command performs.
///
/// The command is the caller's own text, so what reading it can cost is bounded — well
/// above any command an image declares, and far below what would cost this process anything
/// noticeable.
const MAX_COMMAND_COMMANDS: usize = 64;

/// The package manifest a Node program declares its entry module in.
const PACKAGE_MANIFEST: &str = "package.json";

/// One module of a guest program: the path it is stored under inside the artifact, and
/// its source text.
#[derive(Debug, Clone)]
pub struct GuestModule {
    /// The artifact-relative path, with `\` normalised to `/` and any leading `./`
    /// removed, so a `require` of a relative path resolves the name a real runtime
    /// resolves.
    pub path: String,
    /// The module's source text.
    pub source: String,
}

/// Which ENGINE runs a guest's program: the language the module that starts it is written
/// in, which is what the image's own container specification decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestLanguage {
    /// The program is JavaScript, run by the embedded QuickJS engine
    /// ([`super::guest_runtime`]).
    JavaScript,
    /// The program is Python, run by the embedded RustPython engine
    /// ([`super::guest_python`]).
    Python,
}

/// What STARTS a MicroVM's guest, as the image's own container specification declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestStart {
    /// A MODULE of the image, compiled and run by the engine its own suffix names.
    Module {
        /// The path the module is held under in [`GuestProgram::modules`].
        entry: String,
        /// The engine that runs it.
        language: GuestLanguage,
    },
    /// The interpreter's own HTTP FILE-SERVER module, which serves a directory of the
    /// machine's filesystem on the port its invocation names
    /// ([`super::guest_file_server`]) and holds no program of the artifact at all.
    FileServer(ServedModule),
}

/// The program a MicroVM's guest runs: the modules the image holds, the files it holds, and
/// what starts the guest.
#[derive(Debug, Clone)]
pub struct GuestProgram {
    /// The modules an engine can load, keyed by their artifact-relative path.
    pub modules: BTreeMap<String, String>,
    /// The FILES the image holds for its guest, keyed by the ABSOLUTE path each is at: the
    /// build context, under the directory the command runs from, and what the build's own
    /// steps wrote, where each wrote it. Content as well as code, because what a guest
    /// serves is content ([`super::guest_machine`] seeds these as the machine's disk).
    pub files: BTreeMap<String, String>,
    /// What starts the guest.
    pub start: GuestStart,
}

impl GuestProgram {
    /// The path the guest's own command line names: the module that starts it, and the
    /// interpreter's module for a guest that IS that module's server.
    #[must_use]
    pub fn entry(&self) -> &str {
        match &self.start {
            GuestStart::Module { entry, .. } => entry,
            GuestStart::FileServer(_) => HTTP_SERVER_MODULE,
        }
    }
}

/// The guest program `bytes` hold, or `None` when they hold none this emulator can run.
///
/// `key` is the object key the artifact was fetched from, which names the single module
/// of an artifact that is not an archive.
#[must_use]
pub fn read(bytes: &[u8], key: &str) -> Option<GuestProgram> {
    let entries = artifact_entries(bytes, key);
    // What the image's own build steps LEFT in the image: a `RUN` is "a command in a new
    // layer" ([`super::image_build`] performs them), so a context that materialises its
    // guest — its code, and the content that guest serves — declares it there rather than
    // as an entry of the archive, at the absolute path each step wrote.
    let built = super::image_build::perform(bytes, key).files;
    program(entries, built)
}

/// Every entry of the artifact `bytes`, whether or not this engine can load it: the
/// archive's own entries, or the single entry a non-archive object is.
///
/// `key` is the object key the artifact was fetched from, which names that single entry.
/// The build reads the entries this way too — its own instructions live in one of the
/// entries no engine loads ([`super::image_build`]) — so what a build is performed over
/// and what a guest runs come out of ONE read of the artifact.
#[must_use]
pub fn artifact_entries(bytes: &[u8], key: &str) -> Vec<GuestModule> {
    // The bounds this module declares are enforced where the bytes ENTER it, whatever the
    // caller handed over: an object past [`MAX_ARTIFACT_BYTES`] is not read into this
    // process at all, so it holds no entry here.
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ARTIFACT_BYTES {
        return Vec::new();
    }
    match archive_entries(bytes) {
        Some(entries) => entries,
        // Not an archive this emulator reads: the object is the guest's own single
        // module, under the name it was stored as — and one module is held to
        // [`MAX_MODULE_BYTES`] here exactly as an archive's entry is.
        None => String::from_utf8(bytes.to_vec())
            .ok()
            .filter(|source| source.len() <= MAX_MODULE_BYTES)
            .map(|source| {
                vec![GuestModule {
                    path: module_path(key),
                    source,
                }]
            })
            .unwrap_or_default(),
    }
}

/// The program the artifact's `entries` and the files its build steps left (`built`, keyed
/// by the absolute path each was written at) make up.
fn program(entries: Vec<GuestModule>, built: Vec<(String, String)>) -> Option<GuestProgram> {
    // The FILESYSTEM the image holds for its guest: the build context, under the directory
    // the command runs from — which is where a container's context is copied, and where
    // this emulator has always held the artifact's own modules
    // ([`super::guest_machine::GUEST_WORKING_DIRECTORY`]) — and what the steps wrote, at
    // the absolute path each wrote it.
    let working = super::guest_machine::GUEST_WORKING_DIRECTORY;
    let mut files: BTreeMap<String, String> = entries
        .iter()
        .map(|entry| (format!("{working}/{}", entry.path), entry.source.clone()))
        .collect();
    files.extend(built.iter().cloned());
    // Every module an engine can load: an entry of the archive, and one a build step wrote
    // — named the way a command that names either resolves it ([`module_path`]).
    let modules: BTreeMap<String, String> = entries
        .iter()
        .map(|entry| (entry.path.clone(), entry.source.clone()))
        .chain(
            built
                .iter()
                .map(|(path, source)| (module_path(path), source.clone())),
        )
        .filter(|(path, _)| is_loadable(path))
        .collect();
    let start = match declared_start(&entries, &files) {
        // The command starts the interpreter's own server rather than a program of the
        // artifact, so THAT is the guest: it needs no module to have been loaded.
        Some(Declared::FileServer(served)) => GuestStart::FileServer(served),
        declared => {
            let named = match &declared {
                Some(Declared::Module(path)) => Some(path.as_str()),
                _ => None,
            };
            let entry = resolve_entry(&modules, named)?;
            let language = language_of(&entry);
            GuestStart::Module { entry, language }
        }
    };
    Some(GuestProgram {
        modules,
        files,
        start,
    })
}

/// The engine that runs the module stored under `path`, read off its suffix.
fn language_of(path: &str) -> GuestLanguage {
    if path.to_ascii_lowercase().ends_with(PYTHON_SUFFIX) {
        GuestLanguage::Python
    } else {
        GuestLanguage::JavaScript
    }
}

/// Whether the engines can load the module stored under `path`.
fn is_loadable(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    JAVASCRIPT_SUFFIXES
        .iter()
        .chain([PYTHON_SUFFIX].iter())
        .any(|suffix| lowered.ends_with(suffix))
}

/// What the image DECLARES starts its guest.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Declared {
    /// A module it names, resolved against the modules the image holds.
    Module(String),
    /// The interpreter's own HTTP file-server module, with what that invocation serves.
    FileServer(ServedModule),
}

/// What the image DECLARES starts its guest: its container command first — the server that
/// command starts, then the module it names — and then its package manifest's `main`.
///
/// `files` is the filesystem the image holds, which is what the command's own working
/// directory is resolved in ([`server_started_by`]).
fn declared_start(entries: &[GuestModule], files: &BTreeMap<String, String>) -> Option<Declared> {
    let text = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.path == name)
            .map(|entry| entry.source.as_str())
    };
    if let Some(specification) = text(CONTAINER_SPEC) {
        if let Some(served) = container_command_server(specification, files) {
            return Some(Declared::FileServer(served));
        }
        if let Some(module) = container_command_module(specification) {
            return Some(Declared::Module(module));
        }
    }
    text(PACKAGE_MANIFEST)
        .and_then(package_main)
        .map(Declared::Module)
}

/// The arguments of the LAST `CMD` or `ENTRYPOINT` instruction a `Dockerfile` carries (a
/// later one replaces an earlier), which is the command the container RUNS.
///
/// The lines are read through the ONE reading of a container specification's instructions
/// this service has ([`super::image_build::instructions`]), so a command continued over
/// several lines with a trailing `\` is the one instruction the builder reads it as, and a
/// comment line declares nothing.
fn container_command(dockerfile: &str) -> Option<String> {
    super::image_build::instructions(dockerfile)
        .into_iter()
        .filter(|(instruction, _)| COMMAND_INSTRUCTIONS.contains(&instruction.as_str()))
        .map(|(_, arguments)| arguments)
        .next_back()
}

/// The SERVER the image's command starts, where it starts the interpreter's own HTTP
/// file-server module — and `None` for every command that starts something else.
///
/// Both forms the instruction is written in are read: the exec form is the argument vector
/// the container runs, and the shell form is "run in a shell … `/bin/sh -c`", which is also
/// what the exec form spells when it invokes a shell with a program.
fn container_command_server(
    dockerfile: &str,
    files: &BTreeMap<String, String>,
) -> Option<ServedModule> {
    let command = container_command(dockerfile)?;
    match super::image_build::exec_form_words(&command) {
        Some(words) => match shell_program(&words) {
            Some(program) => server_started_by(&program, files),
            None => invoked_server(&words, super::guest_machine::GUEST_WORKING_DIRECTORY),
        },
        None => server_started_by(&command, files),
    }
}

/// The shell PROGRAM an argument vector runs, where the vector invokes a shell with one
/// (`["/bin/sh", "-c", "…"]`), and `None` for a vector that runs a program of its own.
fn shell_program(words: &[String]) -> Option<String> {
    let (name, arguments) = words.split_first()?;
    if !NESTED_SHELL_NAMES.contains(&command_base_name(name)) {
        return None;
    }
    let (option, operands) = arguments.split_first()?;
    (option == SHELL_COMMAND_OPTION)
        .then(|| operands.first().cloned())
        .flatten()
}

/// The server ONE invocation starts: the interpreter's own HTTP file-server module, read
/// through the one reading of that invocation this workspace has
/// (`::service_common::static_web_servers::served_module`, which states the grammar), with
/// `working_directory` the directory the invocation runs in — what the module serves when
/// it names no directory of its own.
///
/// `None` for an invocation of anything else: a program may run an interpreter for
/// anything, and only this module of it starts a server.
fn invoked_server(words: &[String], working_directory: &str) -> Option<ServedModule> {
    let (name, arguments) = words.split_first()?;
    if !INTERPRETERS.contains(&command_base_name(name)) {
        return None;
    }
    served_module(arguments, working_directory)
}

/// The server the shell program `program` starts, read by PERFORMING that program in the
/// bounded, in-process POSIX session this workspace performs caller-supplied shell programs
/// in (`::service_common::posix_shell_session`), over a tree of the image's own `files`.
///
/// Performing it is what makes the DIRECTORY the invocation runs in the shell's own: `cd`
/// before the invocation is where a context that bakes its content somewhere points the
/// server, so the directory is read off the session rather than guessed from the text. The
/// interpreter itself is a program of that environment that RECORDS what its module serves
/// ([`InterpreterPrograms`]) — nothing starts a process, opens a host path or reaches a
/// socket — and the tree is dropped here: this is a reading of the command, not the
/// machine the guest later runs on.
fn server_started_by(program: &str, files: &BTreeMap<String, String>) -> Option<ServedModule> {
    let mut tree =
        ::service_common::posix_shell_volume::ShellVolume::new(super::guest_machine::volume_limits());
    for (path, contents) in files {
        tree.put_file(path, contents);
    }
    let started: Arc<Mutex<Option<ServedModule>>> = Arc::new(Mutex::new(None));
    let mut session = ShellSession::in_volume(BTreeMap::new(), command_config(), tree)
        .with_file_utilities()
        .in_directory(super::guest_machine::GUEST_WORKING_DIRECTORY)
        .with_environment_programs(Box::new(InterpreterPrograms {
            started: Arc::clone(&started),
        }));
    session.run(program);
    let served = started
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    served
}

/// The session the image's command is read in: this reading's own budget, and nothing kept
/// of what the program wrote — the command is read for the server it starts, not for its
/// output.
fn command_config() -> SessionConfig {
    SessionConfig {
        bounds: Bounds {
            commands: MAX_COMMAND_COMMANDS,
            ..Bounds::default()
        },
        output: None,
        ..SessionConfig::default()
    }
}

/// The INTERPRETER, as a program the environment of that session provides: it records the
/// server its own HTTP file-server module was invoked to serve and performs nothing else.
#[derive(Debug)]
struct InterpreterPrograms {
    /// What the last such invocation serves, for the reading that started the session.
    started: Arc<Mutex<Option<ServedModule>>>,
}

impl EnvironmentPrograms for InterpreterPrograms {
    /// Nothing: WHICH directory the module serves depends on the directory the invocation
    /// runs in, so the form this environment answers is the whole invocation below.
    fn run(
        &self,
        _name: &str,
        _arguments: &[String],
        _files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        None
    }

    fn run_invocation(
        &self,
        invocation: &ProgramInvocation<'_>,
        _files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        if !INTERPRETERS.contains(&command_base_name(invocation.name)) {
            return None;
        }
        let served = served_module(invocation.arguments, invocation.working_directory)?;
        *self
            .started
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(served);
        // The server RUNS from here: the status is the one the command carrying it answers,
        // and it writes nothing this reading keeps.
        Some(ProgramStreams {
            status: Statuses::default().success,
            ..ProgramStreams::default()
        })
    }
}

/// The source module a `Dockerfile`'s command names, from the LAST `CMD` or `ENTRYPOINT`
/// instruction it carries.
///
/// Both forms an instruction is written in are read — the JSON array (`CMD ["node",
/// "server.js"]`, `CMD ["/bin/sh", "-c", "python3 /srv/app.py"]`) and the shell form (`CMD
/// node server.js`) — and what is taken from it is the first word naming a module an engine
/// loads. An instruction naming none yields nothing.
fn container_command_module(dockerfile: &str) -> Option<String> {
    container_command(dockerfile)?
        .split(|byte: char| byte.is_ascii_whitespace() || "[],\"'".contains(byte))
        .map(module_path)
        .find(|word| is_loadable(word) && !word.ends_with(".json"))
}

/// The `main` a `package.json` declares, as the path it names.
fn package_main(manifest: &str) -> Option<String> {
    let parsed: ::serde_json::Value = ::serde_json::from_str(manifest).ok()?;
    let main = parsed.get("main")?.as_str()?;
    Some(module_path(main))
}

/// The module that starts the guest: the declared entry when the artifact holds it, then
/// Node's default entry, then the single module of a one-module artifact.
fn resolve_entry(modules: &BTreeMap<String, String>, declared: Option<&str>) -> Option<String> {
    if let Some(declared) = declared.filter(|path| modules.contains_key(*path)) {
        return Some(declared.to_string());
    }
    if modules.contains_key(DEFAULT_ENTRY) {
        return Some(DEFAULT_ENTRY.to_string());
    }
    let mut scripts = modules.keys().filter(|path| !path.ends_with(".json"));
    let only = scripts.next()?;
    scripts.next().is_none().then(|| only.clone())
}

/// The entries of `bytes` read as a `.zip` archive, under this module's bounds — or
/// `None` when the bytes are not an archive this emulator reads.
fn archive_entries(bytes: &[u8]) -> Option<Vec<GuestModule>> {
    let mut archive = ::zip::ZipArchive::new(::std::io::Cursor::new(bytes)).ok()?;
    let mut entries: Vec<GuestModule> = Vec::new();
    let mut left = MAX_PROGRAM_BYTES;
    for index in 0..archive.len().min(MAX_ENTRIES) {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Ok(name) = entry.name() else {
            continue;
        };
        let path = module_path(name.as_ref());
        // The read is capped by what is LEFT of the program budget rather than by the
        // size the archive declares, so an entry that inflates past it stops there.
        let cap = left.min(MAX_MODULE_BYTES);
        let mut held = Vec::new();
        let inflated = entry
            .by_ref()
            .take(u64::try_from(cap).unwrap_or_default().saturating_add(1))
            .read_to_end(&mut held);
        // Every byte the read PRODUCED is charged to the budget, whether or not the entry
        // is kept: inflating an entry past the cap has already cost this process those
        // bytes, so one oversize entry cannot leave the whole budget to the next one —
        // which is what bounds the inflation one archive can drive, however many entries
        // it declares.
        left = left.saturating_sub(held.len());
        if inflated.is_ok() && held.len() <= cap {
            if let Ok(source) = String::from_utf8(held) {
                entries.push(GuestModule { path, source });
            }
        }
        // A budget this archive has spent reads nothing more of it.
        if left == 0 {
            break;
        }
    }
    Some(entries)
}

/// One stored path as a module path: `\` normalised to `/`, any leading `./` removed, and
/// any leading `/` removed — so a module a build step wrote at an absolute path and one
/// the archive holds under a relative path are named the same way, and the command that
/// names either resolves to it.
fn module_path(path: &str) -> String {
    let normalised = path.replace('\\', "/");
    let relative = normalised.strip_prefix("./").unwrap_or(&normalised);
    relative.strip_prefix('/').unwrap_or(relative).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The directory a container's command runs from, which is what the interpreter's
    /// module serves when the invocation names no directory of its own.
    const WORKING: &str = super::super::guest_machine::GUEST_WORKING_DIRECTORY;

    /// RULE: the INFLATED bytes a read of an archive produced are charged to this module's
    /// program budget whether or not the entry they came from is kept, and a spent budget
    /// reads nothing more — so one archive cannot drive unbounded decompression by
    /// declaring entry after entry that inflates past the module cap.
    #[test]
    fn the_inflation_budget_is_charged_by_what_a_read_produced() {
        // (case, how many oversize entries stand in front of the small module, whether
        // that module is still read)
        let rows: Vec<(&str, usize, bool)> = vec![
            ("one oversize entry, with budget left over", 1, true),
            (
                "enough oversize entries to spend the whole budget",
                MAX_PROGRAM_BYTES / MAX_MODULE_BYTES,
                false,
            ),
        ];
        for (case, oversize, read_after) in rows {
            let mut buffer = ::std::io::Cursor::new(Vec::new());
            {
                let mut writer = ::zip::ZipWriter::new(&mut buffer);
                let options: ::zip::write::FileOptions<'_, '_, ()> =
                    ::zip::write::FileOptions::default();
                for ordinal in 0..oversize {
                    writer
                        .start_file(format!("oversize-{ordinal}.js"), options)
                        .expect("the entry starts");
                    ::std::io::Write::write_all(&mut writer, &vec![b'a'; MAX_MODULE_BYTES + 1])
                        .expect("the entry's bytes");
                }
                writer
                    .start_file("late.js", options)
                    .expect("the entry starts");
                ::std::io::Write::write_all(&mut writer, b"// the guest")
                    .expect("the entry's bytes");
                writer.finish().expect("a finished archive");
            }
            let entries = archive_entries(&buffer.into_inner()).unwrap_or_default();
            let held: Vec<(&str, usize)> = entries
                .iter()
                .map(|entry| (entry.path.as_str(), entry.source.len()))
                .collect();
            assert_eq!(
                held.iter().any(|(path, _)| *path == "late.js"),
                read_after,
                "case {case:?}: expected the module behind the oversize entries to be \
                 read={read_after}, got {held:?}"
            );
            assert!(
                !held.iter().any(|(path, _)| path.starts_with("oversize-")),
                "case {case:?}: an entry past the module cap is no module of the program, \
                 got {held:?}"
            );
        }
    }

    /// One row of the entry-module table: the case, the artifact's entries as
    /// `(path, source)` pairs, and the entry that must start the guest.
    type EntryRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, Option<&'a str>);

    /// RULE: the module that starts the guest is the one the artifact DECLARES — the
    /// container command first, then the package manifest's `main`, then Node's default
    /// entry, then the single module of a one-module artifact — and an artifact
    /// declaring none holds no program.
    #[test]
    fn the_entry_module_is_the_one_the_artifact_declares() {
        // (case, the entries, the entry that must start the guest)
        let rows: Vec<EntryRow> = vec![
            (
                "a container command in array form",
                vec![
                    ("Dockerfile", "FROM node:24-alpine\nCMD [\"node\", \"server.js\"]\n"),
                    ("server.js", "// the guest"),
                    ("index.js", "// not the command"),
                ],
                Some("server.js"),
            ),
            (
                "a container command in shell form",
                vec![
                    ("Dockerfile", "FROM node:24-alpine\nENTRYPOINT node app/main.js\n"),
                    ("app/main.js", "// the guest"),
                    ("index.js", "// not the command"),
                ],
                Some("app/main.js"),
            ),
            (
                "the last command instruction wins",
                vec![
                    ("Dockerfile", "CMD [\"node\", \"first.js\"]\nCMD [\"node\", \"second.js\"]\n"),
                    ("first.js", "// replaced"),
                    ("second.js", "// the guest"),
                ],
                Some("second.js"),
            ),
            (
                "a package manifest's main",
                vec![
                    ("package.json", "{\"main\": \"./lib/boot.js\"}"),
                    ("lib/boot.js", "// the guest"),
                    ("index.js", "// not the main"),
                ],
                Some("lib/boot.js"),
            ),
            (
                "no declaration at all",
                vec![("index.js", "// the guest"), ("other.js", "// a module")],
                Some("index.js"),
            ),
            (
                "a single module",
                vec![("guest.js", "// the guest")],
                Some("guest.js"),
            ),
            (
                "several modules and no declaration",
                vec![("a.js", "// one"), ("b.js", "// another")],
                None,
            ),
            (
                "nothing the engine can load",
                vec![("rootfs.img", "\u{0}binary"), ("Dockerfile", "FROM scratch")],
                None,
            ),
            (
                "a declared entry the artifact does not hold",
                vec![
                    ("Dockerfile", "CMD [\"node\", \"absent.js\"]"),
                    ("index.js", "// the guest"),
                ],
                Some("index.js"),
            ),
        ];

        for (case, entries, expected) in rows {
            let entries: Vec<GuestModule> = entries
                .into_iter()
                .map(|(path, source)| GuestModule {
                    path: path.to_string(),
                    source: source.to_string(),
                })
                .collect();
            let actual = program(entries, Vec::new()).map(|program| program.entry().to_string());
            let expected = expected.map(::std::string::ToString::to_string);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the entry {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: what the build's own steps WROTE is a module of the program, so a command
    /// naming a file no entry of the archive holds still resolves to the guest.
    #[test]
    fn a_build_step_can_be_what_declares_the_guest() {
        let dockerfile = "FROM scratch\nRUN mkdir -p /srv\nRUN echo baked > /srv/server.js\n\
                          CMD [\"node\", \"/srv/server.js\"]\n";
        let program = read(dockerfile.as_bytes(), "Dockerfile")
            .expect("the file a build step wrote is the guest program");
        assert_eq!(
            program.start,
            GuestStart::Module {
                entry: "srv/server.js".to_string(),
                language: GuestLanguage::JavaScript,
            },
            "expected the module the command names, run by the engine its suffix names, got \
             {:?} of {:?}",
            program.start,
            program.modules.keys().collect::<Vec<_>>()
        );
    }

    /// One row of the started-server table: the case, the command instruction the image
    /// declares, and the port and directory the server it starts serves — `None` for a
    /// command that starts no such server.
    type ServerRow<'a> = (&'a str, &'a str, Option<(i32, &'a str)>);

    /// RULE: a container command that starts an interpreter's own HTTP FILE-SERVER module
    /// names no file of the artifact: the guest is that server, serving the directory the
    /// command runs in — the one the invocation names, else the one the shell is in when
    /// it runs — on the port the invocation names. Every other command names a module.
    #[test]
    fn an_interpreters_own_server_module_is_what_the_command_starts() {
        // (case, the command instruction, what the guest must be)
        let rows: Vec<ServerRow> = vec![
            (
                "a shell program that changes directory and starts the module",
                "CMD [\"/bin/sh\",\"-c\",\"cd /srv && python3 -m http.server 8080\"]",
                Some((8080, "/srv")),
            ),
            (
                "the module invoked directly, with no port of its own",
                "CMD [\"python3\",\"-m\",\"http.server\"]",
                Some((
                    ::service_common::static_web_servers::DEFAULT_MODULE_PORT,
                    WORKING,
                )),
            ),
            (
                "the module pointed at a directory of its own",
                "CMD [\"python3\",\"-m\",\"http.server\",\"80\",\"--directory\",\"/srv\"]",
                Some((80, "/srv")),
            ),
            (
                "the interpreter by its absolute path, in the shell form",
                "ENTRYPOINT /usr/bin/python3 -m http.server 9000",
                Some((9000, WORKING)),
            ),
            (
                "a directory the image does not hold, so the shell never gets there",
                "CMD [\"/bin/sh\",\"-c\",\"cd /absent && python3 -m http.server 8080\"]",
                None,
            ),
            (
                "another module of the same interpreter",
                "CMD [\"python3\",\"-m\",\"venv\",\"/tmp/env\"]",
                None,
            ),
            (
                "a program of the artifact rather than a module of the interpreter",
                "CMD [\"node\",\"server.js\"]",
                None,
            ),
        ];
        for (case, command, expected) in rows {
            let dockerfile = format!(
                "FROM public.ecr.aws/amazonlinux/amazonlinux:2023\nRUN mkdir -p /srv\n\
                 RUN echo content > /srv/probe.txt\n{command}\n"
            );
            let mut entries = vec![GuestModule {
                path: CONTAINER_SPEC.to_string(),
                source: dockerfile,
            }];
            // A module for the rows whose command names one, so such a row resolves a
            // module rather than nothing at all.
            entries.push(GuestModule {
                path: "server.js".to_string(),
                source: "// the guest".to_string(),
            });
            let built = vec![("/srv/probe.txt".to_string(), "content\n".to_string())];
            let read = program(entries, built).map(|program| program.start);
            let served = match &read {
                Some(GuestStart::FileServer(served)) => {
                    Some((served.port, served.document_root.as_str()))
                }
                _ => None,
            };
            assert_eq!(
                served, expected,
                "case {case:?}: expected the server {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the FILES the image holds for its guest are the build context, under the
    /// directory the command runs from, and what the build's own steps wrote, at the
    /// absolute path each wrote it — content as well as code, so what a static guest
    /// serves is there to serve.
    #[test]
    fn the_image_holds_the_context_and_what_its_steps_wrote() {
        let dockerfile = "FROM public.ecr.aws/amazonlinux/amazonlinux:2023\n\
                          RUN mkdir -p /srv\n\
                          RUN echo baked > /srv/probe.txt\n\
                          CMD [\"/bin/sh\",\"-c\",\"cd /srv && python3 -m http.server 8080\"]\n";
        let program = read(dockerfile.as_bytes(), CONTAINER_SPEC)
            .expect("a container that starts the interpreter's server module holds a guest");
        // (case, the path the image holds the file at, what it carries)
        let rows: Vec<(&str, String, &str)> = vec![
            (
                "what a step wrote, where it wrote it",
                "/srv/probe.txt".to_string(),
                "baked\n",
            ),
            (
                "the context, where the command runs from",
                format!("{WORKING}/{CONTAINER_SPEC}"),
                dockerfile,
            ),
        ];
        for (case, path, expected) in rows {
            assert_eq!(
                program.files.get(&path).map(String::as_str),
                Some(expected),
                "case {case:?}: expected {path} to carry {expected:?}, got {:?}",
                program.files.keys().collect::<Vec<_>>()
            );
        }
    }

    /// RULE: an artifact that is not an archive this emulator reads is the guest's own
    /// single module, under the name the object key gave it.
    #[test]
    fn a_bare_object_is_the_guests_single_module() {
        let program = read(b"// the guest", "team/build/guest.js")
            .expect("a JavaScript object is a one-module guest program");
        assert_eq!(
            program.entry(),
            "team/build/guest.js",
            "expected the object's own key as the entry, got {:?}",
            program.entry()
        );
        assert_eq!(
            program.modules.get("team/build/guest.js").map(String::as_str),
            Some("// the guest"),
            "expected the object's bytes as the module source, got {:?}",
            program.modules
        );
    }
}
