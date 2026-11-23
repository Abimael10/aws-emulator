// SPDX-License-Identifier: CC0-1.0

//! The MODULES a .zip deployment package holds, read out of the bytes the store keeps.
//!
//! A `Zip` function's code is "a .zip file archive" whose entries are the files the
//! handler's module is imported from, so running one starts by reading those entries. The
//! archive is read ENTIRELY in memory: no entry is written to a host path, and nothing
//! outside this process's memory is opened for it.
//!
//! Three bounds are applied while reading, all of them on what this process holds rather
//! than on what the archive DECLARES, because the declared sizes are the caller's:
//!
//! * [`MAX_PACKAGE_ENTRIES`] entries are read at most, so an archive with a very large
//!   directory cannot make a very large read;
//! * [`MAX_UNZIPPED_BYTES`] of INFLATED bytes are read across the whole archive, counted
//!   as they are produced and capped per entry by what is left of it, so a small archive
//!   that inflates to a large one stops at the cap rather than filling memory;
//! * [`MAX_MODULE_BYTES`] of one entry, for the same reason at the level of a single file.
//!
//! Only entries that are SOURCE the engine running the package can compile — one of its
//! suffixes ([`PYTHON_SUFFIXES`], [`JAVASCRIPT_SUFFIXES`]) and valid UTF-8 — are kept as
//! MODULES, because a source file is the only thing this emulator's in-process engines can
//! import: a compiled artefact or a native extension shipped in the package is NOT loaded
//! (nothing here loads a host library), so a handler that needs one fails to import exactly
//! as it would if the file were absent, and nothing is reported as having run.
//!
//! The other entries are the package's own DATA — the files a package ships beside its
//! modules and reads back rather than imports — and are read as that ([`data_entries`]),
//! under the same three bounds and out of what is LEFT of the budget once the modules have
//! been read, so a package's modules are never crowded out by its data.
//!
//! A data entry whose bytes are not text is a file of the package all the same — a layer's
//! archive, an image, a packed table — so it is read as the BYTES it is
//! ([`binary_entries`]), carried as their base64 because every held entry travels to a run as
//! text. Nothing decodes it lossily and nothing imports it: it is a file of the execution
//! environment's filesystem, at its own path under the mount it came from.

use ::base64::Engine as _;

/// One module of a deployment package: the archive-relative path it was stored under and
/// its source text.
#[derive(Debug, Clone)]
pub struct PackageModule {
    /// The entry's path inside the archive, with `\` normalised to `/` and any leading
    /// `./` removed, so `import` resolves the same name a real runtime would resolve.
    pub path: String,
    /// The entry's source text.
    pub source: String,
}

/// Why a deployment package yields no modules to run.
#[derive(Debug, Clone)]
pub enum PackageFault {
    /// This emulator holds no bytes for the package (an S3 object or a container image it
    /// does not resolve), so there is nothing to read.
    NotHeld,
    /// The bytes this emulator holds are not a readable .zip archive.
    Unreadable(String),
}

/// How many entries of one archive are read.
pub const MAX_PACKAGE_ENTRIES: usize = 4_096;

/// How many INFLATED bytes are read across one archive.
pub const MAX_UNZIPPED_BYTES: u64 = 64 * 1024 * 1024;

/// How many INFLATED bytes of ONE entry are read.
pub const MAX_MODULE_BYTES: u64 = 8 * 1024 * 1024;

/// What is left of one INVOCATION's read of its mounts: the entries and the inflated bytes
/// still to be spent across the deployment package and every layer it holds.
///
/// A function's mounts are several archives, so a per-archive bound would multiply by the
/// number of layers the configuration names. This is the one budget they SHARE, carried
/// across the reads of a single invocation and spent as entries are produced, so what a run
/// holds is bounded whatever the configuration lists.
#[derive(Debug)]
pub struct ReadBudget {
    /// How many entries are left to read.
    pub entries: usize,
    /// How many inflated bytes are left to read.
    pub bytes: u64,
}

impl Default for ReadBudget {
    fn default() -> Self {
        Self {
            entries: MAX_PACKAGE_ENTRIES,
            bytes: MAX_UNZIPPED_BYTES,
        }
    }
}

/// The suffixes of an entry the Python interpreter can import.
const PYTHON_SUFFIXES: &[&str] = &[".py"];

/// The suffixes of an entry the JavaScript engine can load: the two module forms Node
/// resolves (`.js`, and the explicit `.mjs`/`.cjs` of an ES and a CommonJS module) and the
/// JSON a package `require`s or imports.
const JAVASCRIPT_SUFFIXES: &[&str] = &[".js", ".mjs", ".cjs", ".json"];

/// Every Python module `zip_bytes` holds, in the archive's own order.
///
/// `Ok(vec![])` is a readable archive with no importable entry in it, which is a package
/// whose handler cannot be imported — not a failure of the read.
pub fn python_modules(zip_bytes: Option<&[u8]>) -> Result<Vec<PackageModule>, PackageFault> {
    source_modules(zip_bytes, PYTHON_SUFFIXES, &mut ReadBudget::default())
}

/// The same, spending `budget` — the read of one archive of an invocation that reads several
/// ([`ReadBudget`]), which is how a RUN reads its mounts whichever language they are in.
pub fn python_modules_within(
    zip_bytes: Option<&[u8]>,
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    source_modules(zip_bytes, PYTHON_SUFFIXES, budget)
}

/// Every DATA entry of a Python deployment package: the files it ships that are not modules
/// the interpreter imports but files the package READS — the certificate bundle, the schema,
/// the table a client library looks up — which the standard library's own resource reading
/// (`importlib.resources`) answers from on a managed runtime.
///
/// Read out of what is LEFT of `budget` after the modules, so the modules of a package whose
/// data is large are read in full. An entry whose bytes are not text is left out for the
/// reason a module's are: nothing here holds the bytes of a file this emulator cannot
/// present, and a package's own file that is absent reads as absent.
pub fn python_resources_within(
    zip_bytes: Option<&[u8]>,
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    data_entries(zip_bytes, PYTHON_SUFFIXES, budget)
}

/// Every entry of a Python deployment package whose bytes are NOT text: the files it ships
/// that no decoding would present as text — an archive, an image, a packed table, a compiled
/// artefact — each carried as the BASE64 of its bytes.
///
/// They are files of the execution environment's filesystem like every other entry: a mount
/// holds one at its own path, so a handler and a program it starts read it and a listing of
/// its directory names it. Nothing imports one and nothing loads one as a library. Read out of
/// what is LEFT of `budget` after the modules and the data above, for the reason those are.
pub fn python_binaries_within(
    zip_bytes: Option<&[u8]>,
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    binary_entries(zip_bytes, budget)
}

/// Every JavaScript (and JSON) module `zip_bytes` holds, in the archive's own order.
///
/// `Ok(vec![])` is a readable archive with no loadable module in it, which is a package
/// whose handler cannot be imported — not a failure of the read.
pub fn javascript_modules(zip_bytes: Option<&[u8]>) -> Result<Vec<PackageModule>, PackageFault> {
    source_modules(zip_bytes, JAVASCRIPT_SUFFIXES, &mut ReadBudget::default())
}

/// The same, spending `budget` — the read of one archive of an invocation that reads several
/// ([`ReadBudget`]).
pub fn javascript_modules_within(
    zip_bytes: Option<&[u8]>,
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    source_modules(zip_bytes, JAVASCRIPT_SUFFIXES, budget)
}

/// Every entry of `zip_bytes` whose path ends in one of `suffixes` and whose bytes are
/// text, read under this module's three bounds.
fn source_modules(
    zip_bytes: Option<&[u8]>,
    suffixes: &[&str],
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    entries_within(zip_bytes, &|path| named_by(path, suffixes), Form::Text, budget)
}

/// Every entry of `zip_bytes` whose path ends in NONE of `suffixes` and whose bytes are
/// text: the package's own data, read under the same bounds.
fn data_entries(
    zip_bytes: Option<&[u8]>,
    suffixes: &[&str],
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    entries_within(zip_bytes, &|path| !named_by(path, suffixes), Form::Text, budget)
}

/// Every entry of `zip_bytes` whose bytes are NOT text, as the base64 of those bytes: the
/// files of the package nothing can present as text, read under the same bounds.
fn binary_entries(
    zip_bytes: Option<&[u8]>,
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    entries_within(zip_bytes, &|_path| true, Form::Bytes, budget)
}

/// WHICH of the entries a read's predicate names it keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The ones whose bytes are text, kept as that text — a module, and a data file a program
    /// reads as text.
    Text,
    /// The ones whose bytes are NOT text, kept as the base64 of those bytes.
    Bytes,
}

/// Whether `path` ends in one of `suffixes`.
fn named_by(path: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| path.ends_with(suffix))
}

/// Every entry of `zip_bytes` that `kept` names and that `form` keeps, read under this
/// module's three bounds.
fn entries_within(
    zip_bytes: Option<&[u8]>,
    kept: &dyn Fn(&str) -> bool,
    form: Form,
    budget: &mut ReadBudget,
) -> Result<Vec<PackageModule>, PackageFault> {
    let Some(zip_bytes) = zip_bytes else {
        return Err(PackageFault::NotHeld);
    };
    let mut archive = ::zip::ZipArchive::new(::std::io::Cursor::new(zip_bytes))
        .map_err(|error| PackageFault::Unreadable(error.to_string()))?;
    let mut modules = Vec::new();
    let entries = archive.len().min(budget.entries);
    for index in 0..entries {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| PackageFault::Unreadable(error.to_string()))?;
        budget.entries = budget.entries.saturating_sub(1);
        if entry.is_dir() {
            continue;
        }
        let Ok(name) = entry.name() else {
            continue;
        };
        let path = normalized_path(name.as_ref());
        if !kept(&path) {
            continue;
        }
        // The read is capped by what is LEFT of the whole-invocation budget and by the
        // per-entry one, never by the size the archive declares for the entry.
        let room = budget.bytes.min(MAX_MODULE_BYTES);
        if room == 0 {
            break;
        }
        let mut bytes = Vec::new();
        ::std::io::Read::read_to_end(&mut ::std::io::Read::take(&mut entry, room), &mut bytes)
            .map_err(|error| PackageFault::Unreadable(error.to_string()))?;
        budget.bytes = budget.bytes.saturating_sub(bytes.len() as u64);
        // A module that is not UTF-8 is not source an engine here can compile; it is
        // left out of a TEXT read rather than lossily decoded into something the caller never
        // wrote, and read as the bytes it is by the read that keeps those ([`Form::Bytes`]).
        match (form, String::from_utf8(bytes)) {
            (Form::Text, Ok(source)) => modules.push(PackageModule { path, source }),
            (Form::Bytes, Err(held)) => modules.push(PackageModule {
                path,
                source: ::base64::engine::general_purpose::STANDARD
                    .encode(held.as_bytes()),
            }),
            _ => continue,
        }
    }
    Ok(modules)
}

/// The archive-relative path an entry is imported under: separators as `/`, and no leading
/// `./` or `/`, so an entry stored either way names the same module.
fn normalized_path(name: &str) -> String {
    let replaced = name.replace('\\', "/");
    let trimmed = replaced.trim_start_matches("./").trim_start_matches('/');
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes of a .zip archive holding `entries`, written in memory.
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = ::std::io::Cursor::new(Vec::new());
        {
            let mut writer = ::zip::ZipWriter::new(&mut buffer);
            let options: ::zip::write::FileOptions<'_, '_, ()> =
                ::zip::write::FileOptions::default();
            for (name, bytes) in entries {
                writer.start_file(*name, options).expect("a new entry");
                ::std::io::Write::write_all(&mut writer, bytes).expect("the entry's bytes");
            }
            writer.finish().expect("a finished archive");
        }
        buffer.into_inner()
    }

    /// One row per kind of entry a package can hold: a module at the root, a module in a
    /// package directory, a path spelled with a leading `./` and with `\` separators, and
    /// the entries that are not importable source at all.
    #[test]
    fn only_the_python_sources_of_an_archive_are_read() {
        let cases: &[(&str, &[u8], Option<&str>)] = &[
            ("lambda_function.py", b"def handler(e, c):\n    return 1\n", Some("lambda_function.py")),
            ("pkg/__init__.py", b"", Some("pkg/__init__.py")),
            ("./nested/mod.py", b"x = 1\n", Some("nested/mod.py")),
            ("win\\mod.py", b"y = 2\n", Some("win/mod.py")),
            ("data.json", b"{}", None),
            ("native.so", b"\x7fELF", None),
            ("binary.py", b"\xff\xfe\x00", None),
        ];
        for (name, bytes, expected) in cases {
            let held = archive(&[(name, bytes)]);
            let modules = python_modules(Some(&held)).expect("a readable archive");
            let actual: Vec<&str> = modules.iter().map(|module| module.path.as_str()).collect();
            let wanted: Vec<&str> = expected.iter().copied().collect();
            assert_eq!(
                actual, wanted,
                "case {name:?}: expected the modules {wanted:?}, got {actual:?}"
            );
        }
    }

    /// The DATA entries of a Python package are exactly the ones that are not modules: one
    /// row per kind of file a package ships beside its modules — at a package directory's
    /// root, at the archive's root, one directory deeper, spelled with a leading `./` and
    /// with `\` separators — and one per entry that is a module or is not text at all.
    #[test]
    fn the_data_entries_of_an_archive_are_the_ones_that_are_not_modules() {
        let cases: &[(&str, &[u8], Option<&str>)] = &[
            (
                "vendored/cacert.pem",
                b"-----BEGIN CERTIFICATE-----\n",
                Some("vendored/cacert.pem"),
            ),
            ("data.json", b"{\"k\":1}", Some("data.json")),
            ("./nested/table.csv", b"id\n1\n", Some("nested/table.csv")),
            ("win\\notes.txt", b"note\n", Some("win/notes.txt")),
            ("lambda_function.py", b"x = 1\n", None),
            ("pkg/__init__.py", b"", None),
            ("blob.bin", b"\xff\xfe\x00", None),
        ];
        for (name, bytes, expected) in cases {
            let held = archive(&[(name, bytes)]);
            let entries = python_resources_within(Some(&held), &mut ReadBudget::default())
                .expect("a readable archive");
            let actual: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
            let wanted: Vec<&str> = expected.iter().copied().collect();
            assert_eq!(
                actual, wanted,
                "case {name:?}: expected the data entries {wanted:?}, got {actual:?}"
            );
        }
    }

    /// One archive holding both, read the way a run reads it: every module is read, and the
    /// text of a data entry is the text the archive stored.
    #[test]
    fn an_archives_modules_and_data_are_both_read() {
        let held = archive(&[
            ("lambda_function.py", b"def handler(e, c):\n    return 1\n"),
            ("vendored/cacert.pem", b"-----BEGIN CERTIFICATE-----\n"),
            ("vendored/__init__.py", b""),
        ]);
        let mut budget = ReadBudget::default();
        let modules = python_modules_within(Some(&held), &mut budget).expect("the modules");
        let data = python_resources_within(Some(&held), &mut budget).expect("the data");
        let module_paths: Vec<&str> = modules.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(
            module_paths,
            vec!["lambda_function.py", "vendored/__init__.py"],
            "expected both modules to be read, got {module_paths:?}"
        );
        let read = data.first().map(|entry| entry.source.as_str());
        assert_eq!(
            read,
            Some("-----BEGIN CERTIFICATE-----\n"),
            "expected the stored text of the data entry back, got {read:?}"
        );
    }

    /// The source read back is the source stored, for an entry of each size class up to
    /// the per-entry cap.
    #[test]
    fn a_modules_source_is_the_source_the_package_holds() {
        for size in [0_usize, 1, 1_024, 64 * 1024] {
            let source = "#".repeat(size);
            let held = archive(&[("m.py", source.as_bytes())]);
            let modules = python_modules(Some(&held)).expect("a readable archive");
            let actual = modules.first().map(|module| module.source.len());
            assert_eq!(
                actual,
                Some(size),
                "case {size} bytes: expected the stored source back, got {actual:?}"
            );
        }
    }

    /// A JavaScript package's entries are the ones the JavaScript engine can load — one
    /// row per suffix a Node package stores a module under, and one per entry that is not
    /// loadable source at all.
    #[test]
    fn only_the_javascript_sources_of_an_archive_are_read() {
        let cases: &[(&str, &[u8], Option<&str>)] = &[
            ("index.js", b"exports.handler = null;\n", Some("index.js")),
            ("index.mjs", b"export const handler = null;\n", Some("index.mjs")),
            ("legacy.cjs", b"module.exports = {};\n", Some("legacy.cjs")),
            ("data.json", b"{\"k\":1}", Some("data.json")),
            ("node_modules/dep/index.js", b"", Some("node_modules/dep/index.js")),
            ("./nested/mod.js", b"", Some("nested/mod.js")),
            ("lambda_function.py", b"x = 1\n", None),
            ("native.node", b"\x7fELF", None),
            ("binary.js", b"\xff\xfe\x00", None),
        ];
        for (name, bytes, expected) in cases {
            let held = archive(&[(name, bytes)]);
            let modules = javascript_modules(Some(&held)).expect("a readable archive");
            let actual: Vec<&str> = modules.iter().map(|module| module.path.as_str()).collect();
            let wanted: Vec<&str> = expected.iter().copied().collect();
            assert_eq!(
                actual, wanted,
                "case {name:?}: expected the modules {wanted:?}, got {actual:?}"
            );
        }
    }

    /// At most [`MAX_PACKAGE_ENTRIES`] entries are read, whatever the archive holds.
    #[test]
    fn no_more_than_the_entry_cap_is_read() {
        let names: Vec<String> = (0..MAX_PACKAGE_ENTRIES + 16)
            .map(|index| format!("m{index}.py"))
            .collect();
        let entries: Vec<(&str, &[u8])> = names
            .iter()
            .map(|name| (name.as_str(), b"x = 1\n" as &[u8]))
            .collect();
        let held = archive(&entries);
        let modules = python_modules(Some(&held)).expect("a readable archive");
        assert_eq!(
            modules.len(),
            MAX_PACKAGE_ENTRIES,
            "expected at most {MAX_PACKAGE_ENTRIES} entries, got {}",
            modules.len()
        );
    }

    /// Bytes that are not an archive, and a package this emulator holds no bytes for, are
    /// each reported as the fault they are rather than as an empty package.
    #[test]
    fn unreadable_bytes_and_an_unheld_package_are_reported() {
        let cases: &[(&str, Option<&[u8]>, bool)] = &[
            ("no bytes held", None, false),
            ("not an archive", Some(b"not a zip at all"), true),
            ("empty bytes", Some(b""), true),
        ];
        for (case, bytes, unreadable) in cases {
            let read = python_modules(*bytes);
            let fault = match read {
                Ok(modules) => panic!("case {case:?}: expected a fault, got {} modules", modules.len()),
                Err(fault) => fault,
            };
            let is_unreadable = matches!(fault, PackageFault::Unreadable(_));
            assert_eq!(
                is_unreadable, *unreadable,
                "case {case:?}: expected unreadable {unreadable}, got {fault:?}"
            );
        }
    }
}
