// SPDX-License-Identifier: CC0-1.0

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    emulator_core_build_support::generate("com_amazonaws_lambda_awsgirapiservice", "smithy-model/com-amazonaws-lambda-awsgirapiservice.json", "src");
    embed_library_sources();
}

/// Where the generated lookup table of the Python standard library's own sources is written,
/// and the name `state::execution::python_library` includes it under.
const LIBRARY_SOURCES_FILE: &str = "python_library_sources.rs";

/// Put the SOURCE of every module of the Python standard library the interpreter ships into
/// this crate, keyed by that module's path inside the library.
///
/// A `python3.x` execution environment holds its managed runtime's standard library as files
/// under the runtime's own library directory, which is what makes a traceback frame inside one
/// of those modules carry the source line it is standing on. The interpreter embedded here
/// imports that library from a FROZEN copy — compiled code objects, no text — so the source
/// those code objects were compiled from is taken here, at build time, from the same library
/// the frozen copy is built out of: the line a frame names is the line the running code really
/// was compiled from, and nothing is read from a host path while a function runs.
///
/// The ONE directory read is the interpreter's own library, at the path that same dependency
/// states for it: no path here comes from a request, from the environment or from anything a
/// caller can steer, and what the build leaves behind is a table of text in this crate rather
/// than a path for the running emulator to open. That directory IS the frozen set — the
/// dependency freezes the whole of it, module for module — so the table is the library the
/// interpreter imports and nothing besides; a file of it that is not a module (its licence,
/// its readme) is left out by taking only `.py` entries.
fn embed_library_sources() {
    let root = Path::new(rustpython_pylib::LIB_PATH);
    let mut held: Vec<(String, PathBuf)> = Vec::new();
    collect_modules(root, root, &mut held);
    assert!(
        !held.is_empty(),
        "the interpreter's own library holds no module under {}, so no frame inside it could \
         carry its source line",
        root.display()
    );
    held.sort();
    let mut rendered = String::from(
        "/// Every module of the standard library the interpreter ships, as (the module's path\n\
         /// inside the library, that module's own source). Sorted by path, which is what\n\
         /// [`library_source`] resolves a frame's module through.\n\
         static LIBRARY_SOURCES: &[(&str, &str)] = &[\n",
    );
    for (path, file) in &held {
        writeln!(
            rendered,
            "    ({path:?}, include_str!({:?})),",
            file.display().to_string()
        )
        .expect("the table renders into a string");
    }
    rendered.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join(LIBRARY_SOURCES_FILE);
    std::fs::write(&out, rendered).expect("the library source table must be written");
}

/// Every `.py` file under `directory`, each with its path relative to `root` — the path a
/// module of the library is held at, and the one a frame of it is resolved by.
///
/// A directory that cannot be read fails the BUILD rather than leaving the table short: a
/// short table is a frame rendered without its source line, which is the defect this table
/// exists to answer.
fn collect_modules(root: &Path, directory: &Path, held: &mut Vec<(String, PathBuf)>) {
    let entries = std::fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "the interpreter's own library must be readable at {}: {error}",
            directory.display()
        )
    });
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| {
                panic!(
                    "the interpreter's own library must be readable at {}: {error}",
                    directory.display()
                )
            })
            .path();
        if path.is_dir() {
            collect_modules(root, &path, held);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "py") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("a file found under the library root is under it");
        // Held under the path the module is at inside the library, with `/` separators
        // whatever the host writes a path with, because that is the path the runtime names a
        // module's file by.
        let relative = relative
            .components()
            .map(|component| component.as_os_str().display().to_string())
            .collect::<Vec<_>>()
            .join("/");
        held.push((relative, path));
    }
}
