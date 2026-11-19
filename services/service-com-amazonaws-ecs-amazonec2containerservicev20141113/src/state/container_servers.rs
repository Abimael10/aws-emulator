// SPDX-License-Identifier: CC0-1.0

//! The SERVER a container's program starts, and what that server answers for a request made to
//! the task's own address on the port it listens on.
//!
//! # The rule this module implements
//!
//! A container's whole purpose is usually to SERVE something. `LoadBalancer$containerPort` is
//! "the port on the container to associate with the load balancer", and `LoadBalancer`'s own
//! documentation says what that binding is for: "the load balancer ... distributes traffic
//! across the tasks that are associated with the service" — so a task of a load-balanced
//! service has to ANSWER on that port, and what answers is the server the container's own
//! program started. The program is the container's `entryPoint`/`command`, "the command that's
//! passed to the container" ([`super::container_shell`]), and a web server image's documented
//! command is exactly an invocation of its server in the foreground.
//!
//! So: a command of this catalog ([`IMAGE_WEB_SERVERS`], or the interpreter's own HTTP server
//! module through [`::service_common::static_web_servers`]) starts a server
//! that LISTENS on the port that server listens on, serving the directory it serves; the task's
//! own address is where it is reached ("tasks that use the `awsvpc` network mode are associated
//! with an elastic network interface", `LoadBalancer$targetGroupArn`), so the server is recorded
//! at that address and port ([`::service_common::served_http_addresses`]) for whatever sends the
//! task traffic — an Elastic Load Balancing v2 `forward` to the `ip` target the task occupies
//! ([`super::load_balancer_targets`]). And the server RUNS: a foreground server does not return,
//! so the command holds the container's program until the container is stopped, which is exactly
//! what keeps such a task `RUNNING` rather than ending the moment its command returned.
//!
//! # What it answers, and why that is real
//!
//! The document it serves is the container's OWN FILESYSTEM ([`super::container_filesystem`]):
//! the file the request's path names under the directory the server serves — the bytes the
//! container's own program wrote there — or the status a static server answers when that
//! directory holds no such file. Nothing is invented for a path nothing wrote, and a port no
//! server of the container listens on is a port nothing answers at.
//!
//! WHICH directory is read off the container's filesystem too: a server ships with one document
//! root in one image and another in the next (the Apache HTTP Server image serves
//! `/usr/local/apache2/htdocs` and the distribution's package `/var/www/html`), so the root is
//! the first of that server's documented candidates the container actually HOLDS — the one its
//! program wrote its page into — and the first candidate for a container that holds none of
//! them, which then serves nothing.
//!
//! WHICH server an invocation names, what its shipped configuration serves, and what an
//! interpreter's own HTTP server module was invoked for are the ONE reading this workspace has
//! ([`::service_common::static_web_servers`]), shared with the Amazon EC2 guest that installs
//! the same servers as packages; and the ONE reading of what a static server ANSWERS for a
//! request is [`::service_com_amazonaws_ec2_amazonec2::state::guest_services::served_by`], which
//! is also what that guest answers a request to a load balancer's target with. This module holds
//! a second copy of neither. What is its own is WHERE the server came from and where it is
//! reached: an instance's own program installs it as a package and the init system starts it,
//! while a container's image ships it, the container's command runs it, and the task's address is
//! what it answers at.
//!
//! # What a server writes when it starts
//!
//! A server that starts says so: it logs that it is up, and the container's log driver delivers
//! those lines like any other output the container's program writes ([`startup_output`]) — so a
//! task running a web server has events in its log stream before any request is made to it.
//! What is delivered is each server's OWN output, and a server whose startup output this
//! emulator does not hold writes nothing: no line is invented for it.
//!
//! # Bounds
//!
//! A server is read as the document root stood when it STARTED, which is when the program that
//! started it stopped writing: the server IS that program, so nothing of the container writes
//! to that root afterwards. How many files and how deep a tree one server reads is bounded
//! ([`MAX_SERVED_FILES`], [`MAX_SERVED_DEPTH`]), under the bound the container's layer already
//! carries ([`super::container_filesystem::CONTAINER_LAYER_LIMITS`]), and how far a command may
//! name the multi-call binary before the applet it runs is bounded too
//! ([`MAX_APPLET_INDIRECTIONS`]). Nothing here opens a socket, starts a process or reaches a
//! host path.

use std::collections::BTreeMap;

use ::service_com_amazonaws_ec2_amazonec2::state::guest_services::{served_by, GuestServer};
use ::service_common::posix_shell_files::ShellFiles;
use ::service_common::served_http_addresses::{
    AddressAnswer, ServedAddressHandle, ServedAddresses, ServesHttp,
};

/// How many files of the container's filesystem ONE server reads. THIS NUMBER IS THE
/// EMULATOR'S: the tree is this process's memory, and the container's own layer holds no more
/// than this anyway ([`super::container_filesystem::CONTAINER_LAYER_LIMITS`]), so a server
/// cannot be made to hold more by the program that started it.
pub const MAX_SERVED_FILES: usize = 256;

/// How deep under the document root one server reads, for the same reason as
/// [`MAX_SERVED_FILES`]: a path deeper than this is not read, so no tree a program writes can
/// make this walk unbounded.
pub const MAX_SERVED_DEPTH: usize = 32;

/// How many DIRECTORIES under the document root one server walks into, for the same reason: a
/// tree whose every entry is a directory holds no file to stop the walk at
/// [`MAX_SERVED_FILES`], and the container's own layer holds no more directories than this
/// anyway ([`super::container_filesystem::CONTAINER_LAYER_LIMITS`]).
pub const MAX_SERVED_DIRECTORIES: usize = 256;

/// One web server an IMAGE ships, as that image's documented command runs it.
///
/// What the server IS — its name, the port its shipped configuration listens on and the
/// document root a distribution's package of it serves — is read through the ONE catalog of
/// static web servers this workspace has
/// ([`::service_common::static_web_servers::WEB_SERVER_PACKAGES`], which an Amazon EC2 guest's
/// own package manager installs from). What is this entry's own is the part that is the IMAGE's
/// rather than the package's: the names a container's command invokes it under, and the
/// document root its own image serves where that differs from the package's.
#[derive(Debug, Clone, Copy)]
pub struct ImageWebServer {
    /// The names a command may invoke it under: the server's own program name, and the
    /// foreground wrappers its official image ships as its command.
    pub invocations: &'static [&'static str],
    /// The server in the shared catalog this names, which carries its name, its port and the
    /// document root the distribution's package serves.
    pub package: &'static str,
    /// The document root the server's OWN image serves, where that differs from the package's —
    /// and `None` where the image serves the same directory.
    pub image_document_root: Option<&'static str>,
}

/// The web servers a container of this emulator can run.
///
/// The names are the two servers' own programs plus the foreground commands their official
/// images are documented to run — `nginx -g 'daemon off;'` for the `nginx` image, and
/// `httpd-foreground` for the `httpd` one, with `apache2-foreground` the same wrapper under the
/// name the Debian-family image carries. The Apache HTTP Server's own image serves
/// `/usr/local/apache2/htdocs` where the distribution's package serves `/var/www/html`, and the
/// `nginx` image serves the same directory its package does, so it names no root of its own.
pub const IMAGE_WEB_SERVERS: &[ImageWebServer] = &[
    ImageWebServer {
        invocations: &["nginx", "nginx-debug"],
        package: "nginx",
        image_document_root: None,
    },
    ImageWebServer {
        invocations: &[
            "httpd",
            "httpd-foreground",
            "apachectl",
            "apache2",
            "apache2-foreground",
        ],
        package: "httpd",
        image_document_root: Some("/usr/local/apache2/htdocs"),
    },
];

/// The option a small server's invocation names its PORT with, and the one it names the
/// directory it serves with.
///
/// BusyBox's own `httpd` usage is `httpd [-ifv[v]] [-c CONFFILE] [-p [IP:]PORT] [-u USER[:GRP]]
/// [-r REALM] [-h HOME]`, so an invocation of it serves `HOME` on `PORT` rather than the
/// directory and port the full server's shipped configuration carries. The applet and the full
/// Apache HTTP Server are invoked under the SAME name — which of the two an image ships is not
/// something this emulator can know — so both options are read only where an operand FOLLOWS
/// them in the form BusyBox's own option takes: a path for the directory and a number for the
/// port. The full server has no option of either letter that takes such an operand (its own `-h`
/// asks for its usage and takes none), so an invocation carrying one is the applet's.
const SERVED_DIRECTORY_OPTION: &str = "-h";
const SERVED_PORT_OPTION: &str = "-p";

/// The multi-call binary whose applets a small image's command invokes through it.
const MULTI_CALL_BINARY: &str = "busybox";

/// How many times one command may name the multi-call binary before the applet it runs.
///
/// THIS NUMBER IS THE EMULATOR'S: the applet name comes out of the argument vector, which is
/// request-controlled, and the form the binary documents (`busybox <applet> [arguments...]`) is
/// ONE indirection — so a vector that repeats it is peeled this far and no further, which is
/// what keeps the reading's cost independent of the length a caller chose.
const MAX_APPLET_INDIRECTIONS: usize = 2;

/// A server one command is STARTING: where it listens, which directories it may serve, and
/// whether it lists a directory that holds no index document.
#[derive(Debug, Clone)]
pub struct StartingServer {
    /// What the server is named, which is what the record of it carries.
    pub program: String,
    /// The port it listens on.
    pub port: i32,
    /// The directories it may serve, most specific first: the one the container HOLDS is the one
    /// it serves ([`Self::document_root`]).
    pub document_roots: Vec<String>,
    /// Whether it answers a DIRECTORY that holds no index document with a listing of what that
    /// directory holds: the interpreter's own HTTP server module does, and a web server image
    /// does not — both ship with that setting as it is here.
    pub lists_directories: bool,
}

impl StartingServer {
    /// The directory this server serves: the first of its candidates the container's filesystem
    /// holds as a directory, and the first candidate otherwise — which then holds nothing, so
    /// the server answers nothing for every path (see this module's header).
    #[must_use]
    pub fn document_root(&self, files: &impl ShellFiles) -> String {
        self.document_roots
            .iter()
            .find(|root| files.is_directory(root))
            .or_else(|| self.document_roots.first())
            .cloned()
            .unwrap_or_default()
    }
}

/// The server the command `name` with `arguments` STARTS, or `None` for a command that starts
/// none.
///
/// `working_directory` is the directory the container's program runs in
/// (`ContainerDefinition$workingDirectory`), which is what the interpreter's module serves when
/// the invocation names no directory of its own.
///
/// A program named by its absolute path is the same program: `/usr/sbin/nginx` and `nginx` are
/// one, as a shell resolving a name on `PATH` finds.
#[must_use]
pub fn server_started_by(
    name: &str,
    arguments: &[String],
    working_directory: &str,
) -> Option<StartingServer> {
    let mut name = name.rsplit('/').next().unwrap_or(name);
    let mut arguments = arguments;
    // A multi-call binary is invoked as itself with the applet's name as its first argument
    // (BusyBox: "it can be invoked as `busybox <applet> [arguments...]`"), which names the same
    // program as the applet's own link does. The applet name comes out of the argument vector, so
    // the indirection is PEELED under a bound of this emulator's ([`MAX_APPLET_INDIRECTIONS`])
    // rather than followed as far as a vector asks: that form describes one indirection, and a
    // vector repeating it past the bound names no program this catalog holds.
    let mut indirections: usize = 0;
    while name == MULTI_CALL_BINARY {
        if indirections >= MAX_APPLET_INDIRECTIONS {
            return None;
        }
        indirections = indirections.saturating_add(1);
        let (applet, rest) = arguments.split_first()?;
        name = applet.rsplit('/').next().unwrap_or(applet);
        arguments = rest;
    }
    if ::service_common::static_web_servers::INTERPRETERS.contains(&name) {
        // The port and the directory the interpreter's own HTTP server module was invoked for,
        // read through the ONE reading of that invocation this workspace has.
        let served =
            ::service_common::static_web_servers::served_module(arguments, working_directory)?;
        return Some(StartingServer {
            program: ::service_common::static_web_servers::HTTP_SERVER_MODULE.to_string(),
            port: served.port,
            document_roots: vec![served.document_root],
            // The module LISTS a directory that holds no index document, which is what
            // `SimpleHTTPRequestHandler` answers such a request with.
            lists_directories: true,
        });
    }
    let image = IMAGE_WEB_SERVERS
        .iter()
        .find(|server| server.invocations.contains(&name))?;
    // What the server IS, out of the shared catalog: its name, the port its shipped
    // configuration listens on, and the document root a distribution's package of it serves.
    let package = ::service_common::static_web_servers::package_named(image.package)?;
    Some(StartingServer {
        program: package.name.to_string(),
        // The port and the directory the INVOCATION names, where it names them
        // ([`SERVED_PORT_OPTION`], [`SERVED_DIRECTORY_OPTION`]), and the shipped configuration's
        // own otherwise.
        port: named_port(arguments).unwrap_or(package.port),
        document_roots: named_directory(arguments).map_or_else(
            || {
                // The image's own root first, where it serves one of its own, then the
                // package's: WHICH of them this container holds is what decides
                // ([`StartingServer::document_root`]).
                image
                    .image_document_root
                    .into_iter()
                    .chain(::std::iter::once(package.document_root))
                    .map(str::to_string)
                    .collect()
            },
            |root| vec![root],
        ),
        lists_directories: false,
    })
}

/// The port an invocation names with [`SERVED_PORT_OPTION`], in either of the forms that option
/// takes (`PORT` and `IP:PORT`) — `None` for an invocation that names none, and for one whose
/// operand is no port a server could listen on
/// ([`::service_common::static_web_servers::SERVED_PORTS`], the ONE range this workspace holds
/// a server to), which leaves the server's own shipped port.
fn named_port(arguments: &[String]) -> Option<i32> {
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word != SERVED_PORT_OPTION {
            continue;
        }
        let operand = words.next()?;
        let port = operand.rsplit(':').next().unwrap_or(operand);
        return port.parse().ok().filter(|port| {
            ::service_common::static_web_servers::SERVED_PORTS.contains(port)
        });
    }
    None
}

/// The directory an invocation names with [`SERVED_DIRECTORY_OPTION`] — `None` for an
/// invocation that names none, including one whose option carries no path operand at all.
fn named_directory(arguments: &[String]) -> Option<String> {
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word != SERVED_DIRECTORY_OPTION {
            continue;
        }
        let operand = words.next()?;
        if !operand.starts_with('/') {
            return None;
        }
        return Some(operand.trim_end_matches('/').to_string());
    }
    None
}

/// The files under `root`, by their absolute path — the documents a server serving that
/// directory can answer with.
///
/// Read off the container's own filesystem, so every entry is one the container's program really
/// wrote, and bounded by [`MAX_SERVED_FILES`], [`MAX_SERVED_DIRECTORIES`] and
/// [`MAX_SERVED_DEPTH`] — the files it holds, the directories it walks into and how deep it goes.
#[must_use]
pub fn files_under(files: &impl ShellFiles, root: &str) -> BTreeMap<String, String> {
    let mut held: BTreeMap<String, String> = BTreeMap::new();
    let mut directories: Vec<(String, usize)> = vec![(root.trim_end_matches('/').to_string(), 0)];
    // How many directories this walk has taken, which is what bounds the walk itself: a tree
    // whose every entry is a directory holds no file to stop it at [`MAX_SERVED_FILES`].
    let mut walked: usize = 0;
    while let Some((directory, depth)) = directories.pop() {
        walked = walked.saturating_add(1);
        if walked > MAX_SERVED_DIRECTORIES {
            return held;
        }
        for name in files.entry_names(&directory) {
            if held.len() >= MAX_SERVED_FILES {
                return held;
            }
            let path = format!("{directory}/{name}");
            if files.is_directory(&path) {
                if depth < MAX_SERVED_DEPTH && directories.len() < MAX_SERVED_DIRECTORIES {
                    directories.push((path, depth.saturating_add(1)));
                }
                continue;
            }
            if let Ok(contents) = files.read(&path) {
                held.insert(path, contents);
            }
        }
    }
    held
}

/// WHERE one container's servers are reachable: the address the task's own elastic network
/// interface holds, and the records the server is published in.
///
/// Handed to the session performing the container's program, which starts a server by
/// publishing it here ([`Self::start`]).
#[derive(Debug, Clone)]
pub struct ContainerServing {
    /// The private address of the interface the task was allocated, which is where a load
    /// balancer reaches the task.
    address: String,
    /// The records of the shared engine, where the server is published for the services that
    /// send this task traffic.
    addresses: ServedAddresses,
}

impl ContainerServing {
    /// The serving of a container whose task is reached at `address`, publishing into
    /// `addresses`.
    #[must_use]
    pub fn new(address: String, addresses: ServedAddresses) -> Self {
        Self { address, addresses }
    }

    /// Start `server` over `files`: the server is reachable at the task's address on its own
    /// port until the handle answered is dropped, which is the server stopping.
    #[must_use]
    pub fn start(
        &self,
        server: GuestServer,
        files: BTreeMap<String, String>,
    ) -> ServedAddressHandle {
        let port = server.port;
        self.addresses.publish(
            &self.address,
            port,
            ::std::sync::Arc::new(ContainerStaticServer { server, files }),
        )
    }
}

/// One server a container is listening with, and the documents it serves.
#[derive(Debug)]
struct ContainerStaticServer {
    /// Which server it is, where it listens and the directory it serves.
    server: GuestServer,
    /// The documents of that directory, as they stood when the server started (see this
    /// module's header).
    files: BTreeMap<String, String>,
}

impl ServesHttp for ContainerStaticServer {
    /// What this server answers, through the ONE reading of what a static server answers for a
    /// request (see this module's header).
    fn serve(&self, method: &str, path: &str) -> Option<AddressAnswer> {
        let answered = served_by(&self.server, &self.files, method, path);
        Some(AddressAnswer {
            status: answered.status,
            content_type: answered.content_type,
            body: answered.body,
        })
    }

    /// Whether the documents this server answers out of are ones this emulator holds: the files
    /// the container's own program wrote into the directory the server serves.
    ///
    /// A server over a document root holding NONE of them serves what the image that ships the
    /// server put there, and a container's own layer "sits over a filesystem the image provides
    /// and this emulator holds none of" ([`super::container_filesystem`]) — so what such a
    /// server answers says nothing about the container, and a reader that judges the container
    /// by a status reads no verdict into it.
    fn holds_what_it_serves(&self) -> bool {
        !self.files.is_empty()
    }
}

/// The server of the shared catalog whose master logs the startup notice below. The catalog
/// carries it under this name (`::service_common::static_web_servers::WEB_SERVER_PACKAGES`),
/// which is the name [`StartingServer::program`] holds for an invocation of it — the same way
/// [`IMAGE_WEB_SERVERS`] names the package each image ships.
const NGINX_SERVER: &str = "nginx";

/// The LEVEL the nginx master's startup notice is logged at, and the notice itself: once the
/// worker processes the master forked are up it logs `start worker processes`, which is the
/// point that server is serving from.
const NGINX_NOTICE_LEVEL: &str = "notice";
const NGINX_WORKERS_STARTED: &str = "start worker processes";

/// How the nginx error log spells the instant of a line — `2026/11/08 19:18:14` — which every
/// line of that log begins with, before the level and the process that wrote it.
const NGINX_LOG_INSTANT: &str = "%Y/%m/%d %H:%M:%S";

/// The process a container's server runs as, and its thread — the pair an nginx error-log line
/// carries as `<pid>#<tid>`.
///
/// One session runs a container's whole program here and forks nothing
/// ([`super::container_shell`]), so the server IS that session: the container's main process,
/// which a container runtime starts as process 1.
const CONTAINER_MAIN_PROCESS: u32 = 1;

/// The address the interpreter's HTTP server module reports itself serving on: the one it binds
/// when its invocation names no other, which is the address it prints.
const MODULE_BOUND_ADDRESS: &str = "0.0.0.0";

/// One line a server writes as it STARTS, and which of the container's two streams it goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupLine {
    /// The text of the line, without the newline that ends it.
    pub text: String,
    /// Whether the server writes it to standard ERROR rather than to standard output.
    pub to_error: bool,
}

/// The lines the server `starting` writes as it begins serving, in the order it writes them.
///
/// Starting a server is not silent, and the container's log driver delivers what the server
/// writes exactly as it delivers everything else the container's program writes. What is
/// written is each server's OWN output and nothing else:
///
/// * the nginx master logs its notices to the error log, each line `<instant> [<level>]
///   <pid>#<tid>: <message>`, and that image routes the error log to the container's standard
///   error — so the notice it logs once its workers are up ([`NGINX_WORKERS_STARTED`]) is one
///   event of the container's stream;
/// * the interpreter's own HTTP server module PRINTS the address and the port it is serving on,
///   which is the one line that invocation writes, on its standard output;
/// * a server whose startup output this emulator does not hold writes nothing here, so a stream
///   holding none is a container whose program wrote none.
///
/// `at` is the instant the server started, which is what the line that carries one reports.
#[must_use]
pub fn startup_output(starting: &StartingServer, at: ::std::time::SystemTime) -> Vec<StartupLine> {
    if starting.program == NGINX_SERVER {
        let instant = ::chrono::DateTime::<::chrono::Utc>::from(at)
            .format(NGINX_LOG_INSTANT)
            .to_string();
        return vec![StartupLine {
            text: format!(
                "{instant} [{NGINX_NOTICE_LEVEL}] {CONTAINER_MAIN_PROCESS}#\
                 {CONTAINER_MAIN_PROCESS}: {NGINX_WORKERS_STARTED}"
            ),
            to_error: true,
        }];
    }
    if starting.program == ::service_common::static_web_servers::HTTP_SERVER_MODULE {
        let port = starting.port;
        return vec![StartupLine {
            text: format!(
                "Serving HTTP on {MODULE_BOUND_ADDRESS} port {port} \
                 (http://{MODULE_BOUND_ADDRESS}:{port}/) ..."
            ),
            to_error: false,
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What one row expects of the command it names: the server's program, the port it listens
    /// on, the first of the directories it may serve and whether it lists one — and `None` for
    /// a command that starts no server at all.
    type Started = Option<(&'static str, i32, &'static str, bool)>;

    /// RULE: a command of the catalog starts a server on the port and directory that server
    /// ships with, and every other command starts none.
    ///
    /// The rows span what the reading generalises over: both servers of the catalog, the
    /// foreground wrapper names their images run them under, a program named by its absolute
    /// path, a small server's own invocation of the port and directory it serves (and the same
    /// applet through the multi-call binary), the interpreter's module with and without a port
    /// and a directory of its own, and the commands that start no server at all (another
    /// invocation of the interpreter, a utility, a name the catalog does not hold).
    #[test]
    fn a_command_of_the_catalog_starts_the_server_its_image_ships() {
        let argv = |words: &[&str]| -> Vec<String> {
            words.iter().map(|word| (*word).to_string()).collect()
        };
        let rows: &[(&str, &str, Vec<String>, Started)] = &[
            (
                "the nginx image's own command",
                "nginx",
                argv(&["-g", "daemon off;"]),
                Some(("nginx", 80, "/usr/share/nginx/html", false)),
            ),
            (
                "the same server named by its absolute path",
                "/usr/sbin/nginx",
                argv(&[]),
                Some(("nginx", 80, "/usr/share/nginx/html", false)),
            ),
            (
                "the Apache HTTP Server image's foreground command",
                "httpd-foreground",
                argv(&[]),
                Some(("httpd", 80, "/usr/local/apache2/htdocs", false)),
            ),
            (
                "the same server under the Debian family's wrapper name",
                "apache2-foreground",
                argv(&[]),
                Some(("httpd", 80, "/usr/local/apache2/htdocs", false)),
            ),
            (
                "the small server's own invocation, which names its port and directory",
                "httpd",
                argv(&["-f", "-v", "-p", "80", "-h", "/www"]),
                Some(("httpd", 80, "/www", false)),
            ),
            (
                "the same applet invoked through the multi-call binary",
                "busybox",
                argv(&["httpd", "-f", "-p", "8080", "-h", "/srv/site"]),
                Some(("httpd", 8080, "/srv/site", false)),
            ),
            (
                "the multi-call binary repeated past the indirections it describes",
                "busybox",
                argv(&["busybox", "busybox", "httpd", "-f", "-p", "80", "-h", "/www"]),
                None,
            ),
            (
                "the multi-call binary naming no applet at all",
                "busybox",
                argv(&[]),
                None,
            ),
            (
                "the same invocation naming the port with an address of its own",
                "httpd",
                argv(&["-f", "-p", "0.0.0.0:8080", "-h", "/www"]),
                Some(("httpd", 8080, "/www", false)),
            ),
            (
                "an invocation naming a port no server could listen on",
                "httpd",
                argv(&["-f", "-p", "70000", "-h", "/www"]),
                Some(("httpd", 80, "/www", false)),
            ),
            (
                "the full server's own option of that letter, which takes no path",
                "httpd-foreground",
                argv(&["-h"]),
                Some(("httpd", 80, "/usr/local/apache2/htdocs", false)),
            ),
            (
                "the interpreter's module on a port and directory it names",
                "python3",
                argv(&["-m", "http.server", "8080", "--directory", "/srv/app"]),
                Some(("http.server", 8080, "/srv/app", true)),
            ),
            (
                "the same module naming neither",
                "python3",
                argv(&["-m", "http.server"]),
                Some(("http.server", 8000, "/work", true)),
            ),
            (
                "the same module on an address it binds",
                "python",
                argv(&["-m", "http.server", "--bind", "0.0.0.0", "80"]),
                Some(("http.server", 80, "/work", true)),
            ),
            (
                "another invocation of the interpreter",
                "python3",
                argv(&["app.py"]),
                None,
            ),
            ("a utility of the session", "echo", argv(&["hello"]), None),
            (
                "a name the catalog does not hold",
                "my-server",
                argv(&["--serve"]),
                None,
            ),
        ];
        for (case, name, arguments, expected) in rows {
            let started = server_started_by(name, arguments, "/work");
            let read = started.as_ref().map(|server| {
                (
                    server.program.clone(),
                    server.port,
                    server
                        .document_roots
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                    server.lists_directories,
                )
            });
            let wanted = expected.map(|(program, port, root, lists)| {
                (program.to_string(), port, root.to_string(), lists)
            });
            assert_eq!(
                read, wanted,
                "case {case:?}: expected {wanted:?}, got {read:?}"
            );
        }
    }

    /// RULE: a server serves the first of its documented document roots the container HOLDS, so
    /// an image that ships the server under one root and one that ships it under another are
    /// both served out of the directory that is really there — and what it answers is read out
    /// of the container's own files.
    ///
    /// The rows span what the rule generalises over: a container holding the first candidate,
    /// one holding only the second, one holding both (the first wins), and one holding neither.
    #[test]
    fn the_document_root_is_the_candidate_the_container_holds() {
        use super::super::container_filesystem::ContainerFilesystem;

        let starting = server_started_by("httpd-foreground", &[], "/")
            .expect("the catalog holds the Apache HTTP Server image's command");
        let rows: &[(&str, &[&str], &str)] = &[
            (
                "a container holding the image's own root",
                &["/usr/local/apache2/htdocs"],
                "/usr/local/apache2/htdocs",
            ),
            (
                "a container holding the distribution package's root",
                &["/var/www/html"],
                "/var/www/html",
            ),
            (
                "a container holding both",
                &["/usr/local/apache2/htdocs", "/var/www/html"],
                "/usr/local/apache2/htdocs",
            ),
            (
                "a container holding neither",
                &[],
                "/usr/local/apache2/htdocs",
            ),
        ];
        for (case, held, expected) in rows {
            let mut filesystem = ContainerFilesystem::default();
            for directory in *held {
                filesystem
                    .write(&format!("{directory}/index.html"), "the page\n", false)
                    .unwrap_or_else(|refusal| {
                        panic!("case {case:?}: the page must be written, got {refusal:?}")
                    });
            }
            let root = starting.document_root(&filesystem);
            assert_eq!(
                root, *expected,
                "case {case:?}: expected the root {expected:?}, got {root:?}"
            );
            let files = files_under(&filesystem, &root);
            let wanted = held.contains(expected);
            assert_eq!(
                files.contains_key(&format!("{expected}/index.html")),
                wanted,
                "case {case:?}: expected the page to be read {wanted}, got {files:?}"
            );
        }
    }
}
