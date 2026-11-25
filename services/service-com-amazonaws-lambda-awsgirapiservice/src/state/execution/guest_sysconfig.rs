// SPDX-License-Identifier: CC0-1.0

//! The CONFIGURATION half of this runtime's standard library: the `sysconfig` module a Python
//! handler — or a library it vendors — imports to ask about the installation it is running on.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library that
//! comes with it: `import sysconfig` imports cleanly on the runtime a handler was written for,
//! and so does every module of the library that imports it on the way up (`zoneinfo`, which
//! asks it where the time-zone files are). A real CPython installation answers those questions
//! out of a GENERATED module its build wrote (`_sysconfigdata_<abi>_<platform>_<multiarch>`),
//! which this interpreter's frozen library has no copy of — so the module is supplied here,
//! answering out of the facts of the execution environment itself.
//!
//! What is this module's own:
//!
//! * the configuration VARIABLES a program reads (`get_config_vars`, `get_config_var`), which
//!   are this runtime's: the installation prefix a managed runtime keeps its language under,
//!   the library directory named for the runtime the function is configured with, the release
//!   the interpreter states and the machine the function's `Architectures` names;
//! * the installation PATHS (`get_paths`, `get_path`, `get_path_names`, the scheme names), laid
//!   out as an installation's are, so a program that asks where modules or headers live is
//!   told the directories this runtime really imports from;
//! * the PLATFORM string (`get_platform`) and the release (`get_python_version`), each read
//!   from the environment the run is in rather than from the machine this process is on.
//!
//! Nothing here reads a host path or a host environment variable: every answer comes from what
//! the invocation's own environment states.

use super::package::PackageModule;

/// The modules of the standard library this half of the runtime supplies: the configuration
/// module.
///
/// Resolved before the frozen standard library and after the deployment package's own entries,
/// so this IS the `sysconfig` a handler imports, and a package shipping its own still wins.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    vec![PackageModule {
        path: "sysconfig.py".to_string(),
        source: SYSCONFIG_SOURCE.to_string(),
    }]
}

/// `sysconfig`: what the installation this runtime IS answers about itself.
const SYSCONFIG_SOURCE: &str = r##""""The configuration of the Python installation this runtime is."""

import os
import sys

__all__ = [
    "get_config_h_filename",
    "get_config_var",
    "get_config_vars",
    "get_default_scheme",
    "get_makefile_filename",
    "get_path",
    "get_path_names",
    "get_paths",
    "get_platform",
    "get_preferred_scheme",
    "get_python_version",
    "get_scheme_names",
    "is_python_build",
    "parse_config_h",
]

# The release this interpreter states, in the two forms the configuration reports it.
_VERSION = "%d.%d" % sys.version_info[:2]
_NODOT = "%d%d" % sys.version_info[:2]

# Where a managed runtime keeps the language installation it ships, and the library directory of
# that installation — named for the runtime the function is configured with, which the
# invocation's own environment states (``AWS_EXECUTION_ENV`` is ``AWS_Lambda_<runtime>``).
_PREFIX = "/var/lang"
_RUNTIME_MARKER = "AWS_Lambda_"
_EXECUTION_ENV = os.environ.get("AWS_EXECUTION_ENV") or ""
if _EXECUTION_ENV.startswith(_RUNTIME_MARKER):
    _RUNTIME = _EXECUTION_ENV[len(_RUNTIME_MARKER):]
else:
    _RUNTIME = "python" + _VERSION
_LIBDIR = _PREFIX + "/lib"
_LIBDEST = _LIBDIR + "/" + _RUNTIME

# The directories a function's own code and the runtime's own modules are imported from, which
# is where this installation's site packages are.
_TASK_ROOT = os.environ.get("LAMBDA_TASK_ROOT") or "/var/task"
_SITE_PACKAGES = _LIBDEST + "/site-packages"

# The machine the execution environment has, which is the one the function's ``Architectures``
# names, and the multiarch triple the implementation states for it.
_MACHINE = getattr(os.uname(), "machine", "") or "x86_64"
_MULTIARCH = getattr(sys.implementation, "_multiarch", "") or _MACHINE + "-linux-gnu"

# The suffix an extension module of this installation is named by, and the ABI tag inside it.
_SOABI = "cpython-%s-%s" % (_NODOT, _MULTIARCH)
_EXT_SUFFIX = "." + _SOABI + ".so"

# The DIRECTORIES the time-zone database of a Linux installation lives in, which is what
# ``zoneinfo`` reads this variable for. This runtime ships its zones with the runtime itself
# rather than as files, so nothing is read from these paths; the value is the one a CPython
# build configures.
_TZPATH = "/usr/share/zoneinfo:/usr/lib/zoneinfo:/usr/share/lib/zoneinfo:/etc/zoneinfo"

_CONFIG_VARS = {
    "abiflags": sys.abiflags if hasattr(sys, "abiflags") else "",
    "AR": "ar",
    "BINDIR": _PREFIX + "/bin",
    "BINLIBDEST": _LIBDEST,
    "CC": "gcc",
    "CXX": "g++",
    "CFLAGS": "",
    "CONFINCLUDEPY": _PREFIX + "/include/" + _RUNTIME,
    "EXE": "",
    "EXT_SUFFIX": _EXT_SUFFIX,
    "INCLUDEDIR": _PREFIX + "/include",
    "INCLUDEPY": _PREFIX + "/include/" + _RUNTIME,
    "LDFLAGS": "",
    "LDLIBRARY": "lib" + _RUNTIME + ".so",
    "LIBDEST": _LIBDEST,
    "LIBDIR": _LIBDIR,
    "MULTIARCH": _MULTIARCH,
    "SCRIPTDIR": _LIBDIR,
    "SHLIB_SUFFIX": ".so",
    "SO": _EXT_SUFFIX,
    "SOABI": _SOABI,
    "TZPATH": _TZPATH,
    "VERSION": _VERSION,
    "WITH_PYMALLOC": 1,
    "base": _PREFIX,
    "exec_prefix": _PREFIX,
    "installed_base": _PREFIX,
    "installed_platbase": _PREFIX,
    "platbase": _PREFIX,
    "platlibdir": "lib",
    "prefix": _PREFIX,
    "projectbase": _PREFIX + "/bin",
    "py_version": "%d.%d.%d" % sys.version_info[:3],
    "py_version_nodot": _NODOT,
    "py_version_nodot_plat": _NODOT,
    "py_version_short": _VERSION,
    "srcdir": _LIBDEST,
    "userbase": None,
}

# The PATHS of an installation, by the names `sysconfig` gives them, for each scheme it
# publishes. A Lambda function's own packages are installed beside its handler at the task
# root, which is the scheme a function's own installation uses.
_PATHS = {
    "stdlib": _LIBDEST,
    "platstdlib": _LIBDEST,
    "purelib": _SITE_PACKAGES,
    "platlib": _SITE_PACKAGES,
    "include": _PREFIX + "/include/" + _RUNTIME,
    "platinclude": _PREFIX + "/include/" + _RUNTIME,
    "scripts": _PREFIX + "/bin",
    "data": _PREFIX,
}

_SCHEMES = {
    "posix_prefix": dict(_PATHS),
    "posix_home": dict(_PATHS),
    "posix_user": dict(_PATHS, purelib=_TASK_ROOT, platlib=_TASK_ROOT),
    "posix_venv": dict(_PATHS),
}


def get_python_version():
    """The release of the interpreter, as the configuration states it."""
    return _VERSION


def get_platform():
    """The platform this installation is built for: the system and the machine it runs on."""
    return "linux-" + _MACHINE


def is_python_build(check_home=None):
    """Whether this is an interpreter running from its own source tree, which it is not."""
    return False


def get_config_vars(*args):
    """Every configuration variable, or the values of the ones named."""
    if not args:
        return dict(_CONFIG_VARS)
    return [_CONFIG_VARS.get(name) for name in args]


def get_config_var(name):
    """One configuration variable, or ``None`` for a name this installation holds none of."""
    return _CONFIG_VARS.get(name)


def get_scheme_names():
    """The installation schemes this installation publishes."""
    return tuple(sorted(_SCHEMES))


def get_default_scheme():
    """The scheme an installation of this platform uses."""
    return "posix_prefix"


def get_preferred_scheme(key):
    """The scheme used for an installation of the kind ``key`` names."""
    if key == "user":
        return "posix_user"
    if key == "home":
        return "posix_home"
    if key == "prefix":
        return "posix_prefix"
    raise ValueError("key must be 'prefix', 'home', or 'user'")


def get_path_names():
    """The names of the paths an installation has."""
    return tuple(_PATHS)


def get_paths(scheme=None, vars=None, expand=True):
    """Every path of one scheme."""
    held = _SCHEMES.get(scheme or get_default_scheme())
    if held is None:
        raise KeyError(scheme)
    paths = dict(held)
    if vars:
        base = vars.get("base") or vars.get("installed_base")
        if base:
            paths = {
                name: path.replace(_PREFIX, base, 1) for name, path in paths.items()
            }
    return paths


def get_path(name, scheme=None, vars=None, expand=True):
    """One path of one scheme."""
    return get_paths(scheme, vars, expand)[name]


def get_config_h_filename():
    """The header the build of this installation wrote its configuration into."""
    return _CONFIG_VARS["CONFINCLUDEPY"] + "/pyconfig.h"


def get_makefile_filename():
    """The makefile the build of this installation used."""
    return _LIBDEST + "/config-" + _VERSION + "/Makefile"


def parse_config_h(fp, vars=None):
    """Read a configuration header, as the module reads one: its ``#define`` lines."""
    if vars is None:
        vars = {}
    for line in fp:
        line = line.strip()
        if not line.startswith("#define"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 2:
            continue
        name = parts[1]
        value = parts[2].strip() if len(parts) > 2 else "1"
        try:
            vars[name] = int(value)
        except ValueError:
            vars[name] = value.strip('"')
    return vars


def expand_makefile_vars(s, vars):
    """Expand the ``$(NAME)`` references of ``s`` from ``vars``."""
    expanded = s
    for name, value in (vars or {}).items():
        expanded = expanded.replace("$(%s)" % name, str(value))
    return expanded


def _get_sysconfigdata_name():
    """The module a CPython build writes its configuration into.

    This installation answers out of its own environment rather than out of a generated
    module, so the name is reported for a program that asks for it and nothing imports it.
    """
    return "_sysconfigdata_%s_%s_%s" % (
        _CONFIG_VARS["abiflags"],
        sys.platform,
        _MULTIARCH,
    )
"##;
