// SPDX-License-Identifier: CC0-1.0

//! The STATIC WEB SERVERS a caller-supplied program can start, and the invocations that start
//! them.
//!
//! Two services of this emulator run a program a caller supplied and then have to answer an
//! HTTP request at the address that program's machine is reached at: an Amazon EC2 instance
//! whose own user data "installs and starts a web server"
//! (`service_com_amazonaws_ec2_amazonec2::state::guest_services`), and an Amazon ECS container
//! whose `command` — "the command that's passed to the container" — runs the server its image
//! ships (`service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::container_servers`).
//! Both ask the same two questions of the same program text: WHICH server one of these
//! invocations names, and WHERE that server listens and serves. This module is the one place
//! those answers live, so the two cannot drift apart — a reading corrected here is corrected
//! for both.
//!
//! What a server is configured to do includes what it SHIPS: a distribution installs a server
//! together with the directory its configuration serves and a page inside it
//! ([`WebServerPackage::default_document`]), so a server that was installed and started, and
//! nothing else, answers its root with that page rather than with nothing. A program that
//! writes its own page at that path replaces it, which is what every tutorial's `echo … >
//! index.html` does.
//!
//! What is NOT here is where each service puts the answer: an instance records it on its own
//! guest's runtime and a container publishes it at its task's address, because that is the part
//! that differs. Nothing here starts a process, opens a socket or reaches a host path: it reads
//! an argument vector and answers what the server it names is configured to do.

/// One web server, as a distribution's own package ships it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebServerPackage {
    /// The name a package manager installs it under, which is also the name its init-system
    /// unit carries.
    pub name: &'static str,
    /// The port its shipped configuration listens on.
    pub port: i32,
    /// The directory its shipped configuration serves.
    pub document_root: &'static str,
    /// The document it ships INSIDE that directory, under [`INDEX_DOCUMENT`]: the page a
    /// freshly installed server answers its root with, before anything of the machine or the
    /// container has written a page of its own there ([`DEFAULT_DOCUMENTS`]).
    pub default_document: &'static str,
}

/// The document the Apache HTTP Server's own distribution ships at its document root: the
/// one-line page its source tree carries as `docs/docroot/index.html`, which is what a server
/// installed from it — the distribution's package, or the official container image — answers a
/// request for its root with until something replaces it.
pub const APACHE_DEFAULT_DOCUMENT: &str = "<html><body><h1>It works!</h1></body></html>\n";

/// The document nginx's own distribution ships at its document root, as its source tree
/// carries it (`html/index.html`) and its package and official container image install it.
pub const NGINX_DEFAULT_DOCUMENT: &str = "<!DOCTYPE html>\n\
     <html>\n\
     <head>\n\
     <title>Welcome to nginx!</title>\n\
     <style>\n\
     html { color-scheme: light dark; }\n\
     body { width: 35em; margin: 0 auto;\n\
     font-family: Tahoma, Verdana, Arial, sans-serif; }\n\
     </style>\n\
     </head>\n\
     <body>\n\
     <h1>Welcome to nginx!</h1>\n\
     <p>If you see this page, the nginx web server is successfully installed and\n\
     working. Further configuration is required.</p>\n\
     \n\
     <p>For online documentation and support please refer to\n\
     <a href=\"http://nginx.org/\">nginx.org</a>.<br/>\n\
     Commercial support is available at\n\
     <a href=\"http://nginx.com/\">nginx.com</a>.</p>\n\
     \n\
     <p><em>Thank you for using nginx.</em></p>\n\
     </body>\n\
     </html>\n";

/// The web servers a program of this emulator can start.
///
/// The Apache HTTP Server package of the Amazon Linux family is `httpd`, serving
/// `/var/www/html` on port 80 — the configuration the EC2 user guide's own web-server
/// tutorial relies on, which installs that package, starts its unit and then writes the page
/// it serves into that directory; `apache2` is the same server under the name the Debian
/// family's package carries; and the `nginx` package serves `/usr/share/nginx/html` on the
/// same port. A package name carrying a version suffix (`httpd24`, `nginx1`) names the same
/// server ([`package_named`]).
///
/// Each ships a PAGE in that directory as well as the directory itself, which is what a server
/// that was installed and started, and nothing else, answers its root with: the two spellings
/// of the Apache HTTP Server are the same server, so they ship the same document
/// ([`APACHE_DEFAULT_DOCUMENT`]), and nginx ships its own ([`NGINX_DEFAULT_DOCUMENT`]).
pub const WEB_SERVER_PACKAGES: &[WebServerPackage] = &[
    WebServerPackage {
        name: "httpd",
        port: 80,
        document_root: "/var/www/html",
        default_document: APACHE_DEFAULT_DOCUMENT,
    },
    WebServerPackage {
        name: "apache2",
        port: 80,
        document_root: "/var/www/html",
        default_document: APACHE_DEFAULT_DOCUMENT,
    },
    WebServerPackage {
        name: "nginx",
        port: 80,
        document_root: "/usr/share/nginx/html",
        default_document: NGINX_DEFAULT_DOCUMENT,
    },
];

/// The document a static server answers a DIRECTORY with, where the directory holds one.
pub const INDEX_DOCUMENT: &str = "index.html";

/// The suffix an init-system unit name carries, which an invocation may leave off.
pub const UNIT_SUFFIX: &str = ".service";

/// The web server `name` names, where this emulator has one of that name — a package name
/// carrying a VERSION suffix (`httpd24`, `nginx1`) names the same server, which is how the
/// Amazon Linux topic and collection packages are written.
#[must_use]
pub fn package_named(name: &str) -> Option<&'static WebServerPackage> {
    let name = name.trim().trim_end_matches(UNIT_SUFFIX);
    WEB_SERVER_PACKAGES.iter().find(|package| {
        name == package.name
            || name
                .strip_prefix(package.name)
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    })
}

/// The interpreters that serve a directory with the module below.
pub const INTERPRETERS: &[&str] = &["python3", "python"];

/// The module a Python interpreter serves a directory over HTTP with, named after `-m`.
pub const HTTP_SERVER_MODULE: &str = "http.server";

/// The port that module serves on when the invocation names none: its own documented default.
pub const DEFAULT_MODULE_PORT: i32 = 8000;

/// The ports a server can be asked to listen on: the range of a TCP port number, which is also
/// the range `PortMapping$containerPort` states ("range 0..65535", with 0 naming no port to
/// listen on). An invocation naming anything outside it names no port a server could be reached
/// at.
pub const SERVED_PORTS: ::std::ops::RangeInclusive<i32> = 1..=65535;

/// What one invocation of [`HTTP_SERVER_MODULE`] serves: the port it listens on and the
/// directory it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedModule {
    /// The port the invocation named, else [`DEFAULT_MODULE_PORT`].
    pub port: i32,
    /// The directory `--directory` named, else the working directory of the session the
    /// command was written in, which the caller passes.
    pub document_root: String,
}

/// The directory and port an interpreter's own HTTP server module was invoked for: `-m
/// http.server`, with the port it was given (else [`DEFAULT_MODULE_PORT`]) and the directory
/// `--directory` names (else `working_directory`).
///
/// `None` for every other invocation of the interpreter: a program may run an interpreter for
/// anything, and only this module of it starts a server — and for one whose port operand is no
/// port a server could listen on ([`SERVED_PORTS`]), which names no server either.
#[must_use]
pub fn served_module(arguments: &[String], working_directory: &str) -> Option<ServedModule> {
    let mut words = arguments.iter();
    if words.next().map(String::as_str) != Some("-m") {
        return None;
    }
    if words.next().map(String::as_str) != Some(HTTP_SERVER_MODULE) {
        return None;
    }
    let mut port = DEFAULT_MODULE_PORT;
    let mut document_root = working_directory.to_string();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--directory" | "-d" => document_root = words.next()?.clone(),
            "--bind" | "-b" => {
                // The interface it binds is not a fact this emulator holds: the machine running
                // it has one address, and the module is serving on it either way.
                let _ = words.next()?;
            }
            operand => {
                port = operand
                    .parse()
                    .ok()
                    .filter(|port| SERVED_PORTS.contains(port))?;
            }
        }
    }
    Some(ServedModule {
        port,
        document_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a row expects of the name it names: the server's name, the port its shipped
    /// configuration listens on and the directory it serves — and `None` for a name that is no
    /// server of this emulator.
    type Shipped = Option<(&'static str, i32, &'static str)>;

    /// What a row expects of an invocation of the module: the port it listens on and the
    /// directory it serves — and `None` for an invocation that serves nothing.
    type Served = Option<(i32, &'static str)>;

    /// RULE: a name is a web server of this emulator exactly when it is one of the packages'
    /// own names, with an init-system unit suffix or a version suffix or neither.
    #[test]
    fn a_package_name_resolves_to_the_server_it_ships() {
        let rows: &[(&str, &str, Shipped)] = &[
            (
                "the Amazon Linux family's Apache package",
                "httpd",
                Some(("httpd", 80, "/var/www/html")),
            ),
            (
                "the same package as an init-system unit",
                "httpd.service",
                Some(("httpd", 80, "/var/www/html")),
            ),
            (
                "the same server under a version suffix",
                "httpd24",
                Some(("httpd", 80, "/var/www/html")),
            ),
            (
                "the Debian family's name for it",
                "apache2",
                Some(("apache2", 80, "/var/www/html")),
            ),
            (
                "the other server",
                "nginx1",
                Some(("nginx", 80, "/usr/share/nginx/html")),
            ),
            ("a name that is no server of this emulator", "lighttpd", None),
            ("a name that only starts like one", "httpdx", None),
            ("no name at all", "", None),
        ];
        for (case, name, expected) in rows {
            let read = package_named(name)
                .map(|package| (package.name, package.port, package.document_root));
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: every server of the catalog ships a page inside the directory its configuration
    /// serves, so one that was installed and started — and had nothing else written for it —
    /// has a document at its root to answer with.
    ///
    /// The rows span the catalog: both spellings of the one server, which ship the same page,
    /// and the other server, which ships its own.
    #[test]
    fn every_server_ships_the_page_its_document_root_holds() {
        let rows: &[(&str, &str, &str)] = &[
            ("the Amazon Linux family's Apache package", "httpd", "It works!"),
            ("the Debian family's name for it", "apache2", "It works!"),
            ("the other server", "nginx", "Welcome to nginx!"),
        ];
        for (case, name, marker) in rows {
            let package = package_named(name)
                .unwrap_or_else(|| panic!("case {case:?}: the catalog must hold {name:?}"));
            assert!(
                package.default_document.contains(marker),
                "case {case:?}: expected the shipped page to carry {marker:?}, got {:?}",
                package.default_document
            );
        }
    }

    /// RULE: the interpreter's own HTTP server module serves the port and the directory the
    /// invocation names, and its documented default port and the session's working directory
    /// where the invocation names neither; every other invocation of the interpreter serves
    /// nothing.
    ///
    /// The rows span what the reading generalises over: both option spellings of the directory,
    /// an invocation naming only the port, one naming only the directory, one naming the
    /// address it binds, one naming nothing, and the invocations that are not this module.
    #[test]
    fn the_interpreters_module_serves_what_its_invocation_names() {
        let argv =
            |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_string()).collect() };
        let rows: &[(&str, Vec<String>, Served)] = &[
            (
                "the port and the directory, long spelling",
                argv(&["-m", "http.server", "8080", "--directory", "/srv/app"]),
                Some((8080, "/srv/app")),
            ),
            (
                "the same, short spelling of the directory",
                argv(&["-m", "http.server", "-d", "/srv/app", "80"]),
                Some((80, "/srv/app")),
            ),
            (
                "the port alone",
                argv(&["-m", "http.server", "9000"]),
                Some((9000, "/work")),
            ),
            (
                "the directory alone",
                argv(&["-m", "http.server", "--directory", "/pages"]),
                Some((DEFAULT_MODULE_PORT, "/pages")),
            ),
            (
                "the address it binds, beside the port",
                argv(&["-m", "http.server", "--bind", "0.0.0.0", "80"]),
                Some((80, "/work")),
            ),
            (
                "neither",
                argv(&["-m", "http.server"]),
                Some((DEFAULT_MODULE_PORT, "/work")),
            ),
            ("another module of the interpreter", argv(&["-m", "venv"]), None),
            ("a program of its own", argv(&["app.py"]), None),
            ("no argument at all", argv(&[]), None),
            (
                "an option with nothing after it",
                argv(&["-m", "http.server", "--directory"]),
                None,
            ),
            (
                "an operand that is no port",
                argv(&["-m", "http.server", "eighty"]),
                None,
            ),
            (
                "an operand outside the ports a server can listen on",
                argv(&["-m", "http.server", "70000"]),
                None,
            ),
            (
                "the port no server listens on",
                argv(&["-m", "http.server", "0"]),
                None,
            ),
        ];
        for (case, arguments, expected) in rows {
            let read = served_module(arguments, "/work")
                .map(|served| (served.port, served.document_root));
            let wanted = expected.map(|(port, root)| (port, root.to_string()));
            assert_eq!(
                read, wanted,
                "case {case:?}: expected {wanted:?}, got {read:?}"
            );
        }
    }
}
