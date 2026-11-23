// SPDX-License-Identifier: CC0-1.0

//! The HTTP CLIENT a container's session provides: the request one container's program — or the
//! probe Amazon ECS runs inside it — makes to the server that same container is running.
//!
//! # The rule this module implements
//!
//! `ContainerDefinition$healthCheck` is "an object representing a container health check", and
//! `HealthCheck$command` is "the command that the container runs to determine if it is healthy"
//! — a command the agent runs INSIDE the container, whose exit code is the result. The command
//! such a check is written with is an HTTP request to the container's own server: the member's
//! own example is `[ "CMD-SHELL", "curl -f http://localhost/ || exit 1" ]`. So a container whose
//! program is serving answers its own probe, and the probe's exit status is whether that server
//! answered the request.
//!
//! WHERE that request goes is the container's own network namespace: a task that uses the
//! `awsvpc` network mode "is allocated its own elastic network interface", and "containers that
//! belong to the same task can communicate over the `localhost` interface"
//! (`NetworkMode`/`AwsVpcConfiguration`). The loopback inside the task and the address of the
//! task's own interface therefore reach the same servers — the ones the containers of that task
//! started ([`super::container_servers`]) — and every other address is one this session has no
//! network to reach, which stays a command it could not run.
//!
//! # Where the document GOES
//!
//! Each client has its own default and its own way of being told otherwise, and a probe written
//! as a pipeline depends on which one it asked for:
//!
//! * `curl` writes what it fetched to its standard output unless it was given a file to save it
//!   in ("`-o`, `--output <file>` — write to file instead of stdout");
//! * `wget` SAVES it, "to a file whose name corresponds to the URL", unless it was given a
//!   name — and the name `-` is standard output itself: "if `-` is used as file, documents will
//!   be printed to standard output" (the GNU Wget manual's `-O file` /
//!   `--output-document=file`), which is what [`STANDARD_OUTPUT_OPERAND`] names.
//!
//! So a document asked for on standard output goes through the session's own stream — the pipe
//! the next command of a pipeline reads, the file a redirection names, or the container's own
//! output — and nothing is saved for it; one asked for by NAME is saved under that name and goes
//! nowhere else.
//!
//! # No socket, no host
//!
//! Nothing here dials anything, resolves a name or reads a host path. The servers are the ones
//! the container's own program published in the shared records
//! ([`::service_common::served_http_addresses`]) — the very records an Elastic Load Balancing v2
//! `forward` and each of its health checks read a target's server out of, so what a container
//! answers its own probe is what the load balancer checking it is answered. The only thing an
//! address is ever used for here is comparing the HOST of a URL a program named against the
//! task's own.
//!
//! The command line itself is read by the workspace's ONE reading of that client's invocation
//! ([`::service_common::instance_metadata_service::Invocation`]), the same one a node's own
//! metadata read is performed through, and the statuses it exits with are that client's
//! documented ones. A request written in a form that reading does not perform runs nothing, like
//! every other command a container's session cannot perform.
//!
//! # Bounds
//!
//! One request carries at most [`MAX_FETCHED_BYTES`], which is this emulator's bound and not a
//! limit of either client: the document is a container's own, so what one fetch takes into this
//! process is bounded here rather than by whatever the server holds.

use ::service_common::instance_metadata_service::{
    Invocation, COULD_NOT_CONNECT, HTTP_RETURNED_ERROR,
};
use ::service_common::served_http_addresses::ServedAddresses;

/// The HTTP client a container's session provides under its own name, the one every documented
/// container health check is written with.
pub const HTTP_CLIENT: &str = "curl";

/// The other client the same request is written with, which the images that ship no `curl` have
/// instead.
pub const HTTP_DOWNLOADER: &str = "wget";

/// How many bytes ONE request carries.
///
/// THIS NUMBER IS THE EMULATOR'S, not a limit of either client: the bytes are the container's own
/// document, so what one fetch takes into this process is bounded here. The same bound the
/// container's other transfer program is held to ([`super::container_aws_cli::MAX_TRANSFERRED_BYTES`]).
pub const MAX_FETCHED_BYTES: usize = 256 * 1024;

/// The status `wget` exits with for an address it could not reach: "4   Network failure" (the GNU
/// Wget manual's exit-status list), which is what nothing listening answers that client.
const WGET_NETWORK_FAILURE: i32 = 4;

/// The status it exits with for a response it reports as an error: "8   Server issued an error
/// response" — which that client reports for every such response, having no option to be asked
/// for one.
const WGET_SERVER_ERROR: i32 = 8;

/// The status `curl` exits with for a document past the bytes it was given to carry: "63
/// Maximum file size exceeded" (the curl manual's exit codes).
const CURL_FILESIZE_EXCEEDED: i32 = 63;

/// The status `wget` exits with for the same, which that client reports as its generic failure:
/// "1   Generic error code".
const WGET_GENERIC_ERROR: i32 = 1;

/// The scheme a request this client performs is made over, and the port each of them is made to
/// where the URL names none — the default port of that scheme.
const HTTP_SCHEME: &str = "http";
const HTTPS_SCHEME: &str = "https";
const HTTP_PORT: i32 = 80;
const HTTPS_PORT: i32 = 443;

/// The HOSTS that name the container's own task: the loopback inside it, in the forms a URL
/// carries one, which is where a container reaches the servers of its own task.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// The document `wget` saves what it fetched under when the invocation names no path and the
/// address names no segment of its own — the name that client gives a document served at a
/// directory.
const WGET_DEFAULT_DOCUMENT: &str = "index.html";

/// The operand that asks a client to write what it fetched to its own output rather than saving
/// it, which is how both of them spell that.
const STANDARD_OUTPUT_OPERAND: &str = "-";

/// What one request of a container's session answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDocument {
    /// The status the client exits with, which is what the program around it reads.
    pub status: i32,
    /// The document it fetched, for a request that got one the client writes.
    pub document: Option<String>,
    /// The path the client was told to SAVE it at, and `None` for one that writes it to its own
    /// output.
    pub save_to: Option<String>,
}

/// One REQUEST a container's session makes, as the words of either client state it.
///
/// The two clients state the same request in their own option grammars, so each is read in its
/// own ([`read_request`]) and what is CARRIED out of both is this: everything the answer is read
/// against.
#[derive(Debug, Clone)]
struct Request {
    /// The method the request is made with.
    method: String,
    /// The address it is made to.
    url: String,
    /// The path the document is saved at, and `None` for a request whose document goes to the
    /// client's own output.
    save_to: Option<String>,
    /// Whether the invocation asked for the document on STANDARD OUTPUT ITSELF, which both
    /// clients spell with [`STANDARD_OUTPUT_OPERAND`] as the name of the file to write — "if
    /// `-` is used as file, documents will be printed to standard output" (the GNU Wget
    /// manual).
    ///
    /// It is what tells that request from one naming no file at all: the downloader SAVES what
    /// it fetched under a name of the URL's own when the invocation names none ([`saved_at`]),
    /// so the two cannot be folded together without the document a probe pipes into another
    /// command going to a file instead.
    to_standard_output: bool,
    /// Whether the client reports an HTTP ERROR RESPONSE as a failure of its own.
    report_error: bool,
    /// Whether the client writes the document at all: the one invocation that asks only whether
    /// the document is THERE writes none ([`WGET_SPIDER_FLAG`]).
    wants_document: bool,
}

/// `arguments` as the request they state, for the client `program` names — `None` for a form
/// that client's reading does not read, which fails closed like every other command a
/// container's session cannot perform.
fn read_request(program: &str, arguments: &[String]) -> Option<Request> {
    if program == HTTP_DOWNLOADER {
        return read_downloader_request(arguments);
    }
    // The flag that asks for everything about the document BUT its body is read here, because it
    // is the METHOD the request is made with, and then kept out of the words the shared reading
    // is given.
    let head = arguments
        .iter()
        .any(|word| CURL_HEAD_FLAGS.contains(&word.as_str()));
    let rest: Vec<String> = arguments
        .iter()
        .filter(|word| !CURL_HEAD_FLAGS.contains(&word.as_str()))
        .cloned()
        .collect();
    // The workspace's ONE reading of that client's command line.
    let invocation = Invocation::read(&rest)?;
    Some(Request {
        method: if head {
            HEAD_METHOD.to_string()
        } else {
            invocation.method.clone()
        },
        url: invocation.url.clone(),
        // That client writes what it fetched to its own output unless it was told to save it.
        save_to: invocation
            .output
            .as_deref()
            .filter(|named| *named != STANDARD_OUTPUT_OPERAND)
            .map(ToString::to_string),
        to_standard_output: invocation.output.as_deref() == Some(STANDARD_OUTPUT_OPERAND),
        report_error: invocation.fail,
        wants_document: true,
    })
}

/// The flags `curl` asks for everything about the document but its body with: "`-I`,
/// `--head` — fetch the headers only", which is the method a static server answers with the
/// document's own status and no body.
const CURL_HEAD_FLAGS: [&str; 2] = ["-I", "--head"];

/// That method's own name.
const HEAD_METHOD: &str = "HEAD";

/// The flag that asks the downloader only whether the document is THERE: "when invoked with
/// this option, Wget will behave as a Web spider, which means that it will not download the
/// pages, just check that they are there" (the GNU Wget manual).
const WGET_SPIDER_FLAG: &str = "--spider";

/// The option the downloader names the path it saves the document at with: "the documents will
/// not be written to the appropriate files, but all will be concatenated together and written to
/// `file`" (`-O file`, `--output-document=file`).
const WGET_OUTPUT_OPTIONS: [&str; 2] = ["-O", "--output-document"];

/// The option it names the METHOD with: "`--method=HTTP-Method` — for the purpose of RESTful
/// scripting, Wget allows sending of other HTTP Methods".
const WGET_METHOD_OPTION: &str = "--method";

/// The valueless flags of the downloader that change nothing about what it was answered: how
/// much it says about itself, whether it checks a certificate, and whether it resumes or keeps a
/// file it already has.
///
/// The LONG spellings and the one short flag of two letters; every other short spelling is read
/// as one letter of a cluster ([`WGET_IGNORED_SHORT_FLAGS`]).
const WGET_IGNORED_FLAGS: [&str; 7] = [
    "--quiet",
    "--verbose",
    "-nv",
    "--no-verbose",
    "--server-response",
    "--no-check-certificate",
    "--continue",
];

/// Its options that carry a value this client has nothing to do with — how long it would wait,
/// how often it would try again, what it would call itself, and a header it would send.
const WGET_IGNORED_VALUED_OPTIONS: [&str; 7] = [
    "--timeout",
    "--connect-timeout",
    "--read-timeout",
    "--tries",
    "--waitretry",
    "--user-agent",
    "--header",
];

/// The SHORT spellings of the valueless flags above, as a cluster of them carries each
/// ([`read_short_options`]): quiet, verbose, continue, and report the server's own response.
const WGET_IGNORED_SHORT_FLAGS: [char; 4] = ['q', 'v', 'c', 'S'];

/// The short spelling of [`WGET_OUTPUT_OPTIONS`].
const WGET_OUTPUT_LETTER: char = 'O';

/// The short spellings of the valued options above, each of which carries a value this client
/// has nothing to do with: the timeout, the retry count, how long it would wait between tries,
/// and what it would call itself.
const WGET_IGNORED_SHORT_VALUED: [char; 4] = ['T', 't', 'w', 'U'];

/// Read one CLUSTER of the downloader's short options — the letters of a word like `-qO-`, past
/// its leading `-`.
///
/// An option that carries a VALUE takes the rest of the word where there is one and the next
/// word otherwise, which is how that client reads both `-O file` and `-Ofile`; the rest of the
/// word is then that value rather than more letters, so the cluster ends there. `None` for a
/// letter this reading does not read, which is a form it performs nothing of.
fn read_short_options(
    cluster: &str,
    words: &mut ::std::slice::Iter<'_, String>,
    named_output: &mut Option<String>,
) -> Option<()> {
    for (at, letter) in cluster.char_indices() {
        if WGET_IGNORED_SHORT_FLAGS.contains(&letter) {
            continue;
        }
        let rest = &cluster[at + letter.len_utf8()..];
        let value = if rest.is_empty() {
            words.next()?.clone()
        } else {
            rest.to_string()
        };
        if letter == WGET_OUTPUT_LETTER {
            *named_output = Some(value);
        } else if !WGET_IGNORED_SHORT_VALUED.contains(&letter) {
            return None;
        }
        // The value took the rest of the word, so this cluster is over.
        return Some(());
    }
    Some(())
}

/// `arguments` as the request the DOWNLOADER states, in that client's own option grammar.
///
/// It saves what it fetched rather than writing it ("by default ... the output is written to a
/// file whose name corresponds to the URL"), so an invocation naming no path still names where
/// the document goes ([`saved_at`]) — while one naming [`STANDARD_OUTPUT_OPERAND`] asked for the
/// document on standard output itself and saves nothing; and it reports every error response as
/// a failure of its own, having no option to be asked for one.
fn read_downloader_request(arguments: &[String]) -> Option<Request> {
    let mut request = Request {
        method: "GET".to_string(),
        url: String::new(),
        save_to: None,
        to_standard_output: false,
        report_error: true,
        wants_document: true,
    };
    let mut named_output: Option<String> = None;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == STANDARD_OUTPUT_OPERAND || !word.starts_with('-') {
            if !request.url.is_empty() {
                return None;
            }
            request.url = word.clone();
            continue;
        }
        if word == WGET_SPIDER_FLAG {
            request.wants_document = false;
            continue;
        }
        if WGET_IGNORED_FLAGS.contains(&word.as_str()) {
            continue;
        }
        if !word.starts_with("--") {
            // A CLUSTER of short options, which that client takes written as one word
            // (`-qO-`), each of which may be the one carrying the rest of it as its value.
            read_short_options(&word[1..], &mut words, &mut named_output)?;
            continue;
        }
        // A long option's value joined to it by `=`, the other spelling every one of them is
        // written in.
        let (option, value) = match word.split_once('=') {
            Some((option, value)) => (option.to_string(), Some(value.to_string())),
            None => (word.clone(), None),
        };
        if WGET_IGNORED_VALUED_OPTIONS.contains(&option.as_str()) {
            if value.is_none() {
                words.next()?;
            }
            continue;
        }
        let value = match value {
            Some(value) => value,
            None => words.next()?.clone(),
        };
        if WGET_OUTPUT_OPTIONS.contains(&option.as_str()) {
            named_output = Some(value);
            continue;
        }
        if option == WGET_METHOD_OPTION {
            request.method = value.to_ascii_uppercase();
            continue;
        }
        // Every other option is one this reading does not read, which is a form it performs
        // nothing of.
        return None;
    }
    if request.url.is_empty() {
        return None;
    }
    request.to_standard_output = named_output.as_deref() == Some(STANDARD_OUTPUT_OPERAND);
    request.save_to = named_output.filter(|named| named != STANDARD_OUTPUT_OPERAND);
    Some(request)
}

/// The HTTP client of ONE container's session: the requests that container's program and probe
/// make to the servers of its own task.
///
/// Built per container run and per health-check probe, and shared with the session performing
/// it, which is why it holds no mutable state of its own.
#[derive(Debug, Clone)]
pub struct ContainerHttpClient {
    /// The private address of the interface the task was allocated, which — beside the loopback
    /// inside the task — is the host a request of this container may name.
    address: String,
    /// The records the servers of this task's containers are published in.
    addresses: ServedAddresses,
    /// Where the bytes this client's requests move are accounted for
    /// ([`super::container_consumption`]) — the accounting of the container's RUN, which the
    /// collections that report its network traffic sample. A client given none counts into a
    /// reading of its own, which nothing samples: the probes the agent runs in a container are
    /// the agent's requests rather than the container's program's.
    accounting: ::std::sync::Arc<super::container_consumption::ContainerConsumption>,
}

impl ContainerHttpClient {
    /// The client of a container whose task is reached at `address`, reading the servers
    /// published in `addresses`.
    #[must_use]
    pub fn new(address: String, addresses: ServedAddresses) -> Self {
        Self {
            address,
            addresses,
            accounting: ::std::sync::Arc::new(
                super::container_consumption::ContainerConsumption::new(),
            ),
        }
    }

    /// This client accounting the bytes its requests move into `accounting` — the accounting of
    /// the container's run ([`super::container_consumption`]).
    #[must_use]
    pub fn accounting_into(
        mut self,
        accounting: ::std::sync::Arc<super::container_consumption::ContainerConsumption>,
    ) -> Self {
        self.accounting = accounting;
        self
    }

    /// What one invocation of `program` answers — `None` for an invocation this client performs
    /// nothing of: a form that client's reading does not read, a scheme it does not make a
    /// request over, or a host that is not this container's own task, which such a container has
    /// no network to reach.
    ///
    /// WHERE the document goes is each client's own (see this module's header): the answer
    /// carries the path it is SAVED at, and none for the one that is written to the session's
    /// standard output.
    ///
    /// A READ: asking a server for a document moves nothing of the container, of the task or of
    /// the records the server is published in. What it does move is BYTES, which are counted
    /// into the container's run as they are sent and received
    /// ([`super::container_consumption`]).
    #[must_use]
    pub fn fetch(&self, program: &str, arguments: &[String]) -> Option<FetchedDocument> {
        let downloader = program == HTTP_DOWNLOADER;
        let request = read_request(program, arguments)?;
        let (port, path) = self.addressed(&request.url)?;
        // The request line this client puts on the connection, which is what it sends.
        self.accounting
            .record_network_sent(request.method.len().saturating_add(path.len()));
        let Some(answered) = self
            .addresses
            .serve(&self.address, port, &request.method, &path)
        else {
            // Nothing of this task is listening there, so the request was never answered:
            // the connection could not be made.
            return Some(FetchedDocument {
                status: if downloader {
                    WGET_NETWORK_FAILURE
                } else {
                    COULD_NOT_CONNECT
                },
                document: None,
                save_to: None,
            });
        };
        // The bytes the server really answered with, whatever the client then does with them.
        self.accounting.record_network_received(answered.body.len());
        if answered.body.len() > MAX_FETCHED_BYTES {
            return Some(FetchedDocument {
                status: if downloader {
                    WGET_GENERIC_ERROR
                } else {
                    CURL_FILESIZE_EXCEEDED
                },
                document: None,
                save_to: None,
            });
        }
        // An error response is a failure of the client's own only where that client reports one
        // as such: the downloader always does, and `curl` only when it was asked to
        // (`-f`/`--fail`). Such a request writes no document.
        if answered.status >= 400 && request.report_error {
            return Some(FetchedDocument {
                status: if downloader {
                    WGET_SERVER_ERROR
                } else {
                    HTTP_RETURNED_ERROR
                },
                document: None,
                save_to: None,
            });
        }
        // The bytes a server answered with, as the text a session carries. A document that is
        // not text is one this session holds nothing of, so nothing of it is written — and nor
        // is one the invocation did not ask for at all.
        let document = request
            .wants_document
            .then(|| String::from_utf8(answered.body).ok())
            .flatten();
        Some(FetchedDocument {
            status: 0,
            document,
            // The downloader SAVES what it fetched, so an invocation of it that named no path
            // still names where the document goes — while one that named standard output
            // itself asked for the document on this session's own stream, which is the pipe,
            // the redirection or the container's output, and saves nothing.
            save_to: match (&request.save_to, request.wants_document) {
                (Some(named), _) => Some(named.clone()),
                (None, true) if downloader && !request.to_standard_output => {
                    Some(saved_at(&path))
                }
                (None, _) => None,
            },
        })
    }

    /// The `(port, path)` of this task's own server that `url` addresses, and `None` for a URL
    /// addressing anything else.
    ///
    /// The address is read by the `http` crate's own URI type — the maintained parser this
    /// workspace already carries for a request line — rather than by anything written here, so a
    /// query string is not part of the path and a URL written without a scheme is read as the
    /// scheme a client assumes for one.
    fn addressed(&self, url: &str) -> Option<(i32, String)> {
        let written = if url.contains("://") {
            url.to_string()
        } else {
            format!("{HTTP_SCHEME}://{url}")
        };
        let parsed = ::http::Uri::try_from(written).ok()?;
        let scheme = parsed.scheme_str().unwrap_or(HTTP_SCHEME);
        let default_port = match scheme {
            HTTP_SCHEME => HTTP_PORT,
            HTTPS_SCHEME => HTTPS_PORT,
            // Every other scheme is one no request of this client is made over.
            _ => return None,
        };
        let host = parsed.host()?.trim_matches(|bracket| bracket == '[' || bracket == ']');
        let own = LOOPBACK_HOSTS
            .iter()
            .any(|loopback| host.eq_ignore_ascii_case(loopback))
            || host.eq_ignore_ascii_case(&self.address);
        if !own {
            return None;
        }
        let port = parsed.port_u16().map_or(default_port, i32::from);
        let path = parsed.path();
        Some((
            port,
            if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            },
        ))
    }
}

/// Where the DOWNLOADER saves a document the invocation named no path for: the last segment of
/// the address it fetched, else the name that client gives a document served at a directory
/// ([`WGET_DEFAULT_DOCUMENT`]).
fn saved_at(path: &str) -> String {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(WGET_DEFAULT_DOCUMENT)
        .to_string()
}

/// Which HTTP client the command `name` invokes, and `None` for a command that invokes neither.
///
/// A program named by its absolute path is the same program: `/usr/bin/curl` and `curl` are one,
/// as a shell resolving a name on `PATH` finds — the same reading a container's own command gets
/// ([`super::container_servers::server_started_by`]).
#[must_use]
pub fn http_client_invoked(name: &str) -> Option<&'static str> {
    match name.rsplit('/').next().unwrap_or(name) {
        HTTP_CLIENT => Some(HTTP_CLIENT),
        HTTP_DOWNLOADER => Some(HTTP_DOWNLOADER),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ::std::sync::Arc;

    use ::service_common::served_http_addresses::{AddressAnswer, ServesHttp};

    use super::*;

    /// The address the task below is reached at, and the port its container serves on.
    const TASK_ADDRESS: &str = "10.4.1.9";
    const SERVED_PORT: i32 = 80;

    /// The server the container of that task is running: it answers its root and one more path,
    /// refuses a third, and answers nothing at all for a fourth — and a `HEAD` of a document it
    /// serves carries everything about it but its body, as a static server answers one.
    #[derive(Debug)]
    struct Serving;

    impl ServesHttp for Serving {
        fn serve(&self, method: &str, path: &str) -> Option<AddressAnswer> {
            let body_wanted = method == "GET";
            let answered = |status: u16, body: &str| {
                Some(AddressAnswer {
                    status,
                    content_type: Some("text/html".to_string()),
                    body: if body_wanted {
                        body.as_bytes().to_vec()
                    } else {
                        Vec::new()
                    },
                })
            };
            if method != "GET" && method != "HEAD" {
                return None;
            }
            match path {
                "/" => answered(200, "the index"),
                "/ping" => answered(200, "pong"),
                "/gone" => answered(404, "not found"),
                _ => None,
            }
        }
    }

    /// One client of a container whose task holds [`TASK_ADDRESS`], with that server listening.
    fn client() -> (ContainerHttpClient, ::service_common::served_http_addresses::ServedAddressHandle)
    {
        let engine = ::engine::InMemoryEngine::new();
        let addresses = ServedAddresses::of(&engine).expect("the engine vends the records");
        let handle = addresses.publish(TASK_ADDRESS, SERVED_PORT, Arc::new(Serving));
        (
            ContainerHttpClient::new(TASK_ADDRESS.to_string(), addresses),
            handle,
        )
    }

    /// The words of one command line, as the session hands them over.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    /// One row of the table below: the case, the client the request is written with, its command
    /// line, the status that client must exit with, and the document it must carry.
    type Row = (
        &'static str,
        &'static str,
        &'static str,
        Option<i32>,
        Option<&'static str>,
    );

    /// RULE: a request a container makes to its OWN task — the loopback inside it or the address
    /// of its own interface — is answered by the server that task's container is running, and the
    /// client exits with what it was answered; every other host is one such a session has no
    /// network to reach.
    ///
    /// The rows span what the rule generalises over: which client the request is written with,
    /// which form of the task's own address it names, the port it names, the path it asks for,
    /// whether the client was asked to report an error response, and the hosts that are not this
    /// task's.
    #[test]
    fn a_container_reaches_the_server_of_its_own_task_and_nothing_else() {
        let (client, _listening) = client();
        let rows: &[Row] = &[
            (
                "the documented health-check request",
                HTTP_CLIENT,
                "-f http://localhost/",
                Some(0),
                Some("the index"),
            ),
            (
                "the loopback address",
                HTTP_CLIENT,
                "-fsS http://127.0.0.1/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the address of the task's own interface",
                HTTP_CLIENT,
                "http://10.4.1.9/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the port the server listens on, named",
                HTTP_CLIENT,
                "-f http://localhost:80/",
                Some(0),
                Some("the index"),
            ),
            (
                "a URL written without a scheme",
                HTTP_CLIENT,
                "-f localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "an error response, with the client asked to report one",
                HTTP_CLIENT,
                "-f http://localhost/gone",
                Some(HTTP_RETURNED_ERROR),
                None,
            ),
            (
                "an error response, with the client asked for nothing",
                HTTP_CLIENT,
                "http://localhost/gone",
                Some(0),
                Some("not found"),
            ),
            (
                "a path the server answers nothing for",
                HTTP_CLIENT,
                "-f http://localhost/nothing-here",
                Some(COULD_NOT_CONNECT),
                None,
            ),
            (
                "a port nothing of the task listens on",
                HTTP_CLIENT,
                "-f http://localhost:8080/",
                Some(COULD_NOT_CONNECT),
                None,
            ),
            (
                "the other client, writing to its own output",
                HTTP_DOWNLOADER,
                "-q -O - http://localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the other client on an error response",
                HTTP_DOWNLOADER,
                "-q -O - http://localhost/gone",
                Some(WGET_SERVER_ERROR),
                None,
            ),
            (
                "the other client where nothing is listening",
                HTTP_DOWNLOADER,
                "-q -O - http://localhost:8080/",
                Some(WGET_NETWORK_FAILURE),
                None,
            ),
            (
                "the other client asked only whether the document is there",
                HTTP_DOWNLOADER,
                "-q --spider http://localhost/",
                Some(0),
                None,
            ),
            (
                "the other client with its destination joined to the option",
                HTTP_DOWNLOADER,
                "-q -O- http://localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the other client naming the method",
                HTTP_DOWNLOADER,
                "-q -O - --method=GET http://localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the other client with a timeout and a retry count",
                HTTP_DOWNLOADER,
                "-q -T 2 --tries=1 -O - http://localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the other client in a form this reading does not read",
                HTTP_DOWNLOADER,
                "--post-data=probe http://localhost/ping",
                None,
                None,
            ),
            (
                "the other client's options written as one cluster",
                HTTP_DOWNLOADER,
                "-qO- http://localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the other client's timeout written as one cluster",
                HTTP_DOWNLOADER,
                "-qT 2 -O - http://localhost/ping",
                Some(0),
                Some("pong"),
            ),
            (
                "the headers of the document alone",
                HTTP_CLIENT,
                "-f -I http://localhost/ping",
                Some(0),
                Some(""),
            ),
            (
                "the headers of a document the server answers nothing for",
                HTTP_CLIENT,
                "-f --head http://localhost/nothing-here",
                Some(COULD_NOT_CONNECT),
                None,
            ),
            ("another host", HTTP_CLIENT, "-f http://example.invalid/", None, None),
            (
                "another address of the account",
                HTTP_CLIENT,
                "-f http://10.4.1.10/",
                None,
                None,
            ),
            (
                "a scheme no request is made over",
                HTTP_CLIENT,
                "-f ftp://localhost/",
                None,
                None,
            ),
        ];
        for (case, program, line, status, document) in rows {
            let fetched = client.fetch(program, &words(line));
            let reported = fetched.as_ref().map(|fetched| fetched.status);
            assert_eq!(
                reported, *status,
                "case {case:?}: expected status {status:?}, got {fetched:?}"
            );
            let carried = fetched
                .as_ref()
                .and_then(|fetched| fetched.document.clone());
            assert_eq!(
                carried.as_deref(),
                *document,
                "case {case:?}: expected document {document:?}, got {carried:?}"
            );
        }
    }

    /// RULE: a client told to SAVE what it fetched names the path it saves at — the one the
    /// invocation named, and for the downloader the address's own last segment where it named
    /// none — while one asked for its own output names none.
    #[test]
    fn a_saved_document_names_the_path_the_invocation_asked_for() {
        let (client, _listening) = client();
        // (case, program, arguments, the path it must save at)
        let rows: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "the client asked to write its own output",
                HTTP_CLIENT,
                "-f http://localhost/ping",
                None,
            ),
            (
                "the client asked to save it",
                HTTP_CLIENT,
                "-f -o /tmp/page http://localhost/ping",
                Some("/tmp/page"),
            ),
            (
                "the client asked for its own output explicitly",
                HTTP_CLIENT,
                "-f -o - http://localhost/ping",
                None,
            ),
            (
                "the downloader, which saves what it fetched",
                HTTP_DOWNLOADER,
                "-q http://localhost/ping",
                Some("ping"),
            ),
            (
                "the downloader on a document served at a directory",
                HTTP_DOWNLOADER,
                "-q http://localhost/",
                Some(WGET_DEFAULT_DOCUMENT),
            ),
            (
                "the downloader asked to save it elsewhere",
                HTTP_DOWNLOADER,
                "-q -O /tmp/held http://localhost/ping",
                Some("/tmp/held"),
            ),
            (
                "the downloader asked for standard output itself",
                HTTP_DOWNLOADER,
                "-q -O - http://localhost/ping",
                None,
            ),
            (
                "the downloader asked for it with the operand joined to the option",
                HTTP_DOWNLOADER,
                "-qO- http://localhost/",
                None,
            ),
            (
                "the downloader asked only whether the document is there",
                HTTP_DOWNLOADER,
                "-q --spider http://localhost/ping",
                None,
            ),
        ];
        for (case, program, line, expected) in rows {
            let fetched = client.fetch(program, &words(line));
            let saved = fetched.as_ref().and_then(|fetched| fetched.save_to.clone());
            assert_eq!(
                saved.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {fetched:?}"
            );
        }
    }

    /// RULE: a program named by its absolute path is the same program a bare name invokes, and
    /// every other name invokes neither client.
    #[test]
    fn a_client_is_invoked_under_its_own_name_however_it_is_written() {
        // (case, the command name, the client it invokes)
        let rows: [(&str, &str, Option<&str>); 6] = [
            ("the client's own name", "curl", Some(HTTP_CLIENT)),
            ("its absolute path", "/usr/bin/curl", Some(HTTP_CLIENT)),
            ("the downloader's name", "wget", Some(HTTP_DOWNLOADER)),
            ("the downloader's path", "/usr/bin/wget", Some(HTTP_DOWNLOADER)),
            ("another program", "nc", None),
            ("a name that only ends in one of them", "mycurl", None),
        ];
        for (case, name, expected) in rows {
            let invoked = http_client_invoked(name);
            assert_eq!(
                invoked, expected,
                "case {case:?}: expected {expected:?}, got {invoked:?}"
            );
        }
    }

    /// RULE: a container whose task has nothing listening at all reaches no server: the client
    /// reports the connection it could not make, which is what a probe of a container that is
    /// not serving yet finds.
    #[test]
    fn nothing_listening_is_a_connection_that_could_not_be_made() {
        let engine = ::engine::InMemoryEngine::new();
        let addresses = ServedAddresses::of(&engine).expect("the engine vends the records");
        let client = ContainerHttpClient::new(TASK_ADDRESS.to_string(), addresses);
        let fetched = client.fetch(HTTP_CLIENT, &words("-f http://localhost/"));
        assert_eq!(
            fetched,
            Some(FetchedDocument {
                status: COULD_NOT_CONNECT,
                document: None,
                save_to: None,
            }),
            "expected the connection the client could not make, got {fetched:?}"
        );
    }
}
