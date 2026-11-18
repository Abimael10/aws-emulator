// SPDX-License-Identifier: CC0-1.0

//! The RESOLVER CLIENTS in the guest of a managed node: the programs a command's script
//! resolves a name with, answered by the DNS of the VPC the node is in.
//!
//! # Why they are installed from here
//!
//! A script a Run Command invocation delivers observes the network from INSIDE the VPC, and
//! the first thing it observes there is DNS: a private hosted zone's records and a Route 53
//! Resolver endpoint's addresses are answered to clients in the VPC and to nobody else, so a
//! client outside it has no way to see either. The programs that ask are `dig` and `nslookup`
//! (`bind-utils`, which the published Amazon Linux images carry), and what they answer comes
//! from the account's own DNS — the private hosted zones associated with the VPC, and the
//! security groups on a Resolver endpoint's interfaces — which is state the service that owns
//! the GUEST cannot read. This crate can
//! ([`::service_com_amazonaws_route53resolver_route53resolver::state::vpc_dns`], the one
//! reading of that data plane), so it installs these programs into the guest's session beside
//! the machine's own ([`super::guest_execution::perform_in_guest`]) rather than either service
//! keeping a second notion of what DNS answers.
//!
//! # What is real here
//!
//! Every answer is the data plane's: which addresses answer DNS in the VPC, whether the
//! security groups on the one addressed admit the query, and what the zones hold for the name.
//! Nothing is echoed from the script, no name resolves because it looks like an address, and a
//! query the data plane says is DROPPED is answered the way a dropped query looks to a client
//! — nothing comes back and the client reports that no server could be reached, which is what
//! makes revoking a rule observable. NOTHING HERE OPENS A SOCKET: the query never leaves this
//! process, and the server address is only ever compared against the addresses the VPC's DNS
//! answers at.
//!
//! The lines a real client writes about the TRANSPORT — how long the query took, when it was
//! sent, how many bytes came back — are not written at all: no packet was carried, so there is
//! no measurement to report, and a number invented for them would be the one thing in this
//! output that nothing computed.

use std::time::{SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_route53resolver_route53resolver::state::vpc_dns::{
    Answer, Resolution, ResponseCode, VpcDnsView, DNS_PORT, PROTOCOL_TCP, PROTOCOL_UDP, TYPE_A,
    TYPE_AAAA,
};
use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramOutcome,
};

/// The program that queries a name server directly — "DNS lookup utility" — which is what a
/// script resolving a name through a named server runs.
pub const QUERY_CLIENT: &str = "dig";

/// The program that looks a name up through the host's own resolver configuration.
pub const LOOKUP_CLIENT: &str = "nslookup";

/// The BIND release the resolver clients of this emulator's guests report themselves as, which
/// a client writes in the first line of its own output.
///
/// ONE build for the whole emulator, so every guest reports the client this emulator emulates
/// rather than a per-image guess — the same reading the managed nodes' agent version takes
/// ([`super::node_registration::SSM_AGENT_VERSION`]).
pub const CLIENT_VERSION: &str = "9.18.33";

/// The status `dig` exits with when nothing answered it: "9" is what it reports when "no
/// servers could be reached" (dig(1), Return Codes), which is the status a client sees for a
/// query that was dropped as much as for one no server was listening for.
pub const NO_SERVERS_REACHED: i32 = 9;

/// The status a lookup exits with when it could not resolve the name: `nslookup` reports a
/// failure of its own as 1, whatever the reason the resolver gave.
pub const LOOKUP_FAILED: i32 = 1;

/// The one line either client writes when nothing answered it, which is what a script branches
/// on.
pub const NO_SERVERS_LINE: &str = ";; no servers could be reached";

/// The status either client exits with for a query that was answered.
const ANSWERED: i32 = 0;

/// How many times a client tries a server that does not answer before it gives up: "3" is
/// dig's own default (`+tries=3`), and each try is a line of its own in the output.
const DEFAULT_TRIES: usize = 3;

/// The class every record of this data plane is in: the internet class, which is the only one
/// Route 53 holds records in.
const CLASS_INTERNET: &str = "IN";

/// How many queries ONE invocation of a client performs, however many names it was given.
///
/// The arguments are a caller's text, so what one command can ask for is bounded here — on
/// EVERY spelling that adds a name ([`Invocation::push_name`]), since the two spellings are the
/// same input to the same loop.
const MAX_QUERIES_PER_INVOCATION: usize = 16;

/// How many lines ONE invocation of a client writes, across every query it performed.
///
/// The bound on the PRODUCT the arguments and the zones multiply out to: a query is a block of
/// output and an answered record is a line of it, so neither the number of names nor the number
/// of records one name answers with bounds the output on its own. An invocation that reached it
/// writes the lines it had, which is what a client whose output was cut off writes.
const MAX_OUTPUT_LINES_PER_INVOCATION: usize = 1_024;

/// The record types a client reads as a TYPE rather than as a name, from Route 53's own
/// `RRType` enum (`smithy_query(query="shape",shape="RRType")` against the Route 53 model),
/// plus the `ANY` pseudo-type a client may ask for.
const RECORD_TYPES: &[&str] = &[
    "SOA", "A", "TXT", "NS", "CNAME", "MX", "NAPTR", "PTR", "SRV", "SPF", "AAAA", "CAA", "DS",
    "TLSA", "SSHFP", "SVCB", "HTTPS", "ANY",
];

/// The resolver clients installed in one node's guest, holding the VPC DNS view their answers
/// come from and nothing else — no store guard, no engine and no host handle is reachable from
/// a program.
#[derive(Debug)]
pub struct ResolverPrograms {
    /// One instant's reading of the DNS the node can reach, taken before the script ran.
    view: VpcDnsView,
}

/// The resolver clients for the guest whose DNS `view` describes.
#[must_use]
pub fn resolver_programs(view: VpcDnsView) -> Box<dyn EnvironmentPrograms> {
    Box::new(ResolverPrograms { view })
}

impl EnvironmentPrograms for ResolverPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        _files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        match name {
            QUERY_CLIENT => self.queried(arguments),
            LOOKUP_CLIENT => self.looked_up(arguments),
            // Every other program is one this catalog does not provide.
            _ => None,
        }
    }
}

impl ResolverPrograms {
    /// One invocation of the query client: a block of output per name it was given.
    ///
    /// `None` for an invocation written in a form this client does not read, which the session
    /// then reports as a command it could not run — the same closed failure every other
    /// program of a guest answers for one.
    fn queried(&self, arguments: &[String]) -> Option<ProgramOutcome> {
        let invocation = Invocation::read(arguments)?;
        let server = invocation
            .server
            .clone()
            .unwrap_or_else(|| self.view.nameserver());
        let mut output: Vec<String> = Vec::new();
        let mut status = ANSWERED;
        for name in &invocation.names {
            if output.len() >= MAX_OUTPUT_LINES_PER_INVOCATION {
                break;
            }
            match self
                .view
                .answer(&server, name, &invocation.record_type, invocation.protocol())
            {
                Resolution::Answered(answer) => {
                    if invocation.short {
                        output.extend(short_answer(&answer));
                    } else {
                        output.extend(full_answer(&invocation, &server, name, &answer));
                    }
                }
                // A query that was dropped and a query nothing was listening for look the same
                // to a client: nothing came back.
                Resolution::Dropped | Resolution::NoServer => {
                    output.extend(unanswered(&invocation, &server, name));
                    status = NO_SERVERS_REACHED;
                }
            }
        }
        output.truncate(MAX_OUTPUT_LINES_PER_INVOCATION);
        Some(ProgramOutcome { status, output })
    }

    /// One invocation of the lookup client: `nslookup [-option …] NAME [SERVER]`.
    ///
    /// Its options are read, not passed over: what type to ask for is one of them, and a lookup
    /// whose `-type=` this client dropped on the floor would answer a DIFFERENT query from the
    /// one it was given. An option this client does not read performs nothing at all, the same
    /// closed failure the query client answers for one.
    fn looked_up(&self, arguments: &[String]) -> Option<ProgramOutcome> {
        let lookup = Lookup::read(arguments)?;
        let server = lookup
            .server
            .clone()
            .unwrap_or_else(|| self.view.nameserver());
        let name = lookup.name.clone();
        let mut output = vec![
            format!("Server:\t\t{server}"),
            format!("Address:\t{server}#{DNS_PORT}"),
            String::new(),
        ];
        let answered = self
            .view
            .answer(&server, &name, &lookup.record_type, lookup.protocol());
        let status = match answered {
            Resolution::Answered(answer) => match answer.code {
                ResponseCode::NoError if !answer.records.is_empty() => {
                    if !answer.authoritative {
                        output.push("Non-authoritative answer:".to_string());
                    }
                    for record in answer.records.iter().take(MAX_OUTPUT_LINES_PER_INVOCATION) {
                        output.push(format!("Name:\t{}", bare_name(&record.name)));
                        output.push(format!("{}\t{}", lookup.value_label(), record.value));
                    }
                    ANSWERED
                }
                code => {
                    output.push(format!(
                        "** server can't find {}: {}",
                        bare_name(&name),
                        code.as_str()
                    ));
                    LOOKUP_FAILED
                }
            },
            Resolution::Dropped | Resolution::NoServer => {
                let mut lines = timed_out(&server, DEFAULT_TRIES);
                lines.push(NO_SERVERS_LINE.to_string());
                output = lines;
                LOOKUP_FAILED
            }
        };
        output.truncate(MAX_OUTPUT_LINES_PER_INVOCATION);
        Some(ProgramOutcome { status, output })
    }
}

/// One invocation of the LOOKUP client, as its arguments state it.
#[derive(Debug)]
struct Lookup {
    /// The name asked for.
    name: String,
    /// The server named after it, or `None` to ask the one the guest's resolver configuration
    /// holds.
    server: Option<String>,
    /// The type asked for (`-type=`, `-querytype=`, `-q=`), `A` where the invocation names none.
    record_type: String,
    /// Whether the query is to be carried on a CONNECTION (`-vc`).
    over_tcp: bool,
}

/// The `-option=VALUE` settings of the lookup client that name the TYPE to ask for, which are
/// four spellings of one setting.
const LOOKUP_TYPE_OPTIONS: &[&str] = &["-type", "-querytype", "-query", "-q"];

/// Its `-option=VALUE` settings about the TRANSPORT this client has none of — how long it would
/// wait and how often it would try — read and ignored with their values.
const LOOKUP_IGNORED_OPTIONS: &[&str] = &["-timeout", "-retry", "-ndots"];

/// Its valueless settings that change nothing about what is answered here.
const LOOKUP_IGNORED_FLAGS: &[&str] = &["-debug", "-nodebug", "-sil", "-silent"];

impl Lookup {
    /// `arguments` as one lookup, or `None` for a form this client does not perform: an option
    /// it does not read, a class or a port it does not serve, no name at all, or more words than
    /// a name and a server.
    fn read(arguments: &[String]) -> Option<Self> {
        let mut name: Option<String> = None;
        let mut server: Option<String> = None;
        let mut record_type = TYPE_A.to_string();
        let mut over_tcp = false;
        for argument in arguments {
            if !argument.starts_with('-') {
                if name.is_none() {
                    name = Some(argument.clone());
                } else if server.is_none() {
                    server = Some(argument.clone());
                } else {
                    return None;
                }
                continue;
            }
            if argument == "-vc" {
                over_tcp = true;
                continue;
            }
            if LOOKUP_IGNORED_FLAGS.contains(&argument.as_str()) {
                continue;
            }
            let (option, value) = match argument.split_once('=') {
                Some((option, value)) => (option, value),
                // Every other setting of this client carries a value, so a bare one is a form
                // it does not read.
                None => return None,
            };
            if LOOKUP_TYPE_OPTIONS.contains(&option) {
                if !RECORD_TYPES
                    .iter()
                    .any(|known| value.eq_ignore_ascii_case(known))
                {
                    // A type no record of this data plane has is a form this client does not
                    // perform, rather than a query answered as though it had named another.
                    return None;
                }
                record_type = value.to_ascii_uppercase();
                continue;
            }
            if LOOKUP_IGNORED_OPTIONS.contains(&option) {
                continue;
            }
            // The internet class is the only one these records are in, and DNS is served at the
            // one port: a setting naming another is a form this client does not perform.
            match option {
                "-class" if value.eq_ignore_ascii_case(CLASS_INTERNET) => continue,
                "-port" if value == DNS_PORT.to_string() => continue,
                _ => return None,
            }
        }
        Some(Self {
            name: name?,
            server,
            record_type,
            over_tcp,
        })
    }

    /// The protocol the lookup travels over, which is what the security groups on a Resolver
    /// endpoint's interfaces are asked about.
    fn protocol(&self) -> &'static str {
        if self.over_tcp {
            PROTOCOL_TCP
        } else {
            PROTOCOL_UDP
        }
    }

    /// The label this client writes a value under: an ADDRESS is reported as one, and the value
    /// of every other type is reported as the record data it is.
    fn value_label(&self) -> &'static str {
        if self.record_type == TYPE_A || self.record_type == TYPE_AAAA {
            "Address:"
        } else {
            "Value:"
        }
    }
}

/// One invocation of the query client, as its arguments state it.
#[derive(Debug, Default)]
struct Invocation {
    /// The server the query is sent to (`@ADDRESS`), or `None` to query the one the guest's
    /// resolver configuration names.
    server: Option<String>,
    /// The names asked for, in the order they were written.
    names: Vec<String>,
    /// The record type asked for, `A` where the invocation names none — which is the type a
    /// client asks for by default.
    record_type: String,
    /// Whether only the answer's own values are to be written (`+short`), which is the form a
    /// script reads an answer in.
    short: bool,
    /// Whether the query is to be carried on a CONNECTION (`+tcp`, `+vc`) rather than in a
    /// datagram, which is a different port in the security groups that gate it.
    over_tcp: bool,
}

/// The options that carry a value and are READ: the record type, and the name for an
/// invocation that spells it as an option.
const VALUED_OPTIONS: &[&str] = &["-t", "-q", "-c"];

/// The `+option` settings this client reads and acts on.
const SHORT_OPTION: &str = "+short";

/// The `+option` settings that ask for the query to go over a connection.
const TCP_OPTIONS: &[&str] = &["+tcp", "+vc"];

/// The `+option` settings about the TRANSPORT this client has none of — how long it would
/// wait, how often it would try, whether it would fall back — each read and ignored, with or
/// without a value.
const IGNORED_OPTION_PREFIXES: &[&str] = &[
    "+time",
    "+tries",
    "+retry",
    "+timeout",
    "+notcp",
    "+novc",
    "+nocookie",
    "+nostats",
    "+nocmd",
    "+noedns",
    "+edns",
    "+bufsize",
];

impl Invocation {
    /// `arguments` as one invocation, or `None` for a form this client does not perform.
    ///
    /// Fails CLOSED, like every other command a guest's session cannot perform: an option this
    /// client does not read runs nothing rather than being answered as though it had been read.
    fn read(arguments: &[String]) -> Option<Self> {
        let mut invocation = Self {
            record_type: TYPE_A.to_string(),
            ..Self::default()
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            if let Some(server) = argument.strip_prefix('@') {
                if invocation.server.is_some() || server.is_empty() {
                    return None;
                }
                invocation.server = Some(server.to_string());
                continue;
            }
            if argument == SHORT_OPTION {
                invocation.short = true;
                continue;
            }
            if TCP_OPTIONS.contains(&argument.as_str()) {
                invocation.over_tcp = true;
                continue;
            }
            if argument.starts_with('+') {
                let read = IGNORED_OPTION_PREFIXES
                    .iter()
                    .any(|prefix| argument == prefix || argument.starts_with(&format!("{prefix}=")));
                if read {
                    continue;
                }
                return None;
            }
            if VALUED_OPTIONS.contains(&argument.as_str()) {
                let value = arguments.next()?.clone();
                match argument.as_str() {
                    "-t" => invocation.record_type = value.to_ascii_uppercase(),
                    "-q" => invocation.push_name(value)?,
                    // `-c CLASS`: the internet class is the only one these records are in, and
                    // any other is a form this client does not perform.
                    _ if value.eq_ignore_ascii_case(CLASS_INTERNET) => {}
                    _ => return None,
                }
                continue;
            }
            if argument.starts_with('-') {
                return None;
            }
            // A bare word is the TYPE when it names one, and the name otherwise — the order
            // the client reads `dig NAME TYPE` in.
            if RECORD_TYPES
                .iter()
                .any(|known| argument.eq_ignore_ascii_case(known))
            {
                invocation.record_type = argument.to_ascii_uppercase();
                continue;
            }
            if invocation.names.len() >= MAX_QUERIES_PER_INVOCATION {
                return None;
            }
            invocation.push_name(argument.clone())?;
        }
        if invocation.names.is_empty() {
            return None;
        }
        Some(invocation)
    }

    /// Add one name to the queries this invocation performs, or `None` once it has as many as
    /// one invocation performs ([`MAX_QUERIES_PER_INVOCATION`]) — the ONE place a name is added,
    /// so both spellings of the same input are bounded identically.
    fn push_name(&mut self, name: String) -> Option<()> {
        if self.names.len() >= MAX_QUERIES_PER_INVOCATION {
            return None;
        }
        self.names.push(name);
        Some(())
    }

    /// The protocol the query travels over, which is what the security groups on a Resolver
    /// endpoint's interfaces are asked about.
    fn protocol(&self) -> &'static str {
        if self.over_tcp {
            PROTOCOL_TCP
        } else {
            PROTOCOL_UDP
        }
    }
}

/// The `+short` form of an answer: the VALUES of its records, one per line and nothing else.
fn short_answer(answer: &Answer) -> Vec<String> {
    answer
        .records
        .iter()
        .map(|record| record.value.clone())
        .collect()
}

/// The full form: the client's own banner, the header the answer carried, the question, and
/// the answer section.
fn full_answer(
    invocation: &Invocation,
    server: &str,
    name: &str,
    answer: &Answer,
) -> Vec<String> {
    let asked = displayed_name(name);
    let mut lines = banner(invocation, server, name);
    lines.push(";; Got answer:".to_string());
    lines.push(format!(
        ";; ->>HEADER<<- opcode: QUERY, status: {}, id: {}",
        answer.code.as_str(),
        query_id(name)
    ));
    lines.push(format!(
        ";; flags: qr{} rd ra; QUERY: 1, ANSWER: {}, AUTHORITY: 0, ADDITIONAL: 0",
        if answer.authoritative { " aa" } else { "" },
        answer.records.len()
    ));
    lines.push(String::new());
    lines.push(";; QUESTION SECTION:".to_string());
    lines.push(format!(
        ";{asked}\t\t\t{CLASS_INTERNET}\t{}",
        invocation.record_type
    ));
    if !answer.records.is_empty() {
        lines.push(String::new());
        lines.push(";; ANSWER SECTION:".to_string());
        for record in &answer.records {
            lines.push(format!(
                "{}\t{}\t{CLASS_INTERNET}\t{}\t{}",
                displayed_name(&record.name),
                record.ttl,
                record.record_type,
                record.value
            ));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        ";; SERVER: {server}#{DNS_PORT}({server}) ({})",
        if invocation.over_tcp { "TCP" } else { "UDP" }
    ));
    lines
}

/// What the client writes for a query nothing came back for: one line per try, then the
/// banner, then the one line a script branches on.
fn unanswered(invocation: &Invocation, server: &str, name: &str) -> Vec<String> {
    let mut lines = timed_out(server, DEFAULT_TRIES);
    lines.extend(banner(invocation, server, name));
    lines.push(NO_SERVERS_LINE.to_string());
    lines
}

/// The try lines themselves: a query that was never answered looks like this however it was
/// lost.
fn timed_out(server: &str, tries: usize) -> Vec<String> {
    (0..tries)
        .map(|_| format!(";; communications error to {server}#{DNS_PORT}: timed out"))
        .collect()
}

/// The two lines every invocation of the query client opens with, naming itself and what it
/// was asked.
fn banner(invocation: &Invocation, server: &str, name: &str) -> Vec<String> {
    vec![
        String::new(),
        format!(
            "; <<>> DiG {CLIENT_VERSION} <<>> @{server} {name} {}",
            invocation.record_type
        ),
        format!("; ({} server found)", 1),
        ";; global options: +cmd".to_string(),
    ]
}

/// A name as a client WRITES it: fully qualified, which is the form the data plane holds one
/// in.
fn displayed_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.ends_with('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

/// A name as the LOOKUP client writes it: without the root dot, which is the form that client
/// reports a name and a name it could not find in.
fn bare_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_string()
}

/// The identifier of one query, which the header reports.
///
/// A query's id is an arbitrary 16-bit number a client picks per query, so this one is picked
/// per query too: the instant it was asked at, mixed with the name, so two queries of one
/// script do not report the same id. Nothing reads it back — it is the client's own value, not
/// a fact of the answer.
fn query_id(name: &str) -> u16 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or_default();
    let mut mixed = u64::from(nanos);
    for byte in name.bytes() {
        mixed = mixed.rotate_left(7) ^ u64::from(byte);
    }
    // splitmix64's finalizer, so the digits of one id do not read as the clock's.
    mixed ^= mixed >> 30;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    u16::try_from(mixed & 0xffff).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the reading table: the arguments, and what the client reads out of them.
    struct ReadingRow {
        case: &'static str,
        arguments: &'static [&'static str],
        /// What was read, or `None` for a form the client performs nothing of.
        read: Option<Read>,
    }

    /// What one invocation was read AS, as a row states it.
    #[derive(Debug, PartialEq, Eq)]
    struct Read {
        server: Option<&'static str>,
        names: &'static [&'static str],
        record_type: &'static str,
        short: bool,
        over_tcp: bool,
    }

    /// RULE: an invocation of the query client is read as its arguments state it, and a form
    /// the client does not read performs nothing at all.
    ///
    /// The rows span what the reading generalises over: where the server is named, where the
    /// type is (a bare word, `-t`), the `+short` form, the connection form, several names, an
    /// option about the transport that is read and ignored, and the forms that fail closed —
    /// including more names than one invocation performs, in either spelling.
    #[test]
    fn the_query_clients_arguments_are_read_as_written() {
        let past_the_cap: Vec<String> = (0..=MAX_QUERIES_PER_INVOCATION)
            .map(|index| format!("name-{index}.example.internal"))
            .collect();
        let past_the_cap_as_options: Vec<String> = past_the_cap
            .iter()
            .flat_map(|name| ["-q".to_string(), name.clone()])
            .collect();
        let rows = [
            ReadingRow {
                case: "a name alone",
                arguments: &["app.example.internal"],
                read: Some(Read {
                    server: None,
                    names: &["app.example.internal"],
                    record_type: "A",
                    short: false,
                    over_tcp: false,
                }),
            },
            ReadingRow {
                case: "a server, a name and a bare type",
                arguments: &["@169.254.169.253", "app.example.internal", "AAAA"],
                read: Some(Read {
                    server: Some("169.254.169.253"),
                    names: &["app.example.internal"],
                    record_type: "AAAA",
                    short: false,
                    over_tcp: false,
                }),
            },
            ReadingRow {
                case: "the type as an option, lower case",
                arguments: &["app.example.internal", "-t", "cname"],
                read: Some(Read {
                    server: None,
                    names: &["app.example.internal"],
                    record_type: "CNAME",
                    short: false,
                    over_tcp: false,
                }),
            },
            ReadingRow {
                case: "the short form",
                arguments: &["+short", "@10.91.1.10", "app.example.internal"],
                read: Some(Read {
                    server: Some("10.91.1.10"),
                    names: &["app.example.internal"],
                    record_type: "A",
                    short: true,
                    over_tcp: false,
                }),
            },
            ReadingRow {
                case: "the connection form",
                arguments: &["+tcp", "app.example.internal"],
                read: Some(Read {
                    server: None,
                    names: &["app.example.internal"],
                    record_type: "A",
                    short: false,
                    over_tcp: true,
                }),
            },
            ReadingRow {
                case: "several names, in both spellings",
                arguments: &["app.example.internal", "-q", "www.example.internal"],
                read: Some(Read {
                    server: None,
                    names: &["app.example.internal", "www.example.internal"],
                    record_type: "A",
                    short: false,
                    over_tcp: false,
                }),
            },
            ReadingRow {
                case: "an option about the transport, with its value",
                arguments: &["+time=2", "+tries=1", "app.example.internal"],
                read: Some(Read {
                    server: None,
                    names: &["app.example.internal"],
                    record_type: "A",
                    short: false,
                    over_tcp: false,
                }),
            },
            ReadingRow {
                case: "no name at all",
                arguments: &["@10.91.1.10"],
                read: None,
            },
            ReadingRow {
                case: "an option this client does not read",
                arguments: &["-x", "10.91.1.10"],
                read: None,
            },
            ReadingRow {
                case: "a + option this client does not read",
                arguments: &["+trace", "app.example.internal"],
                read: None,
            },
            ReadingRow {
                case: "two servers",
                arguments: &["@10.91.1.10", "@10.91.2.10", "app.example.internal"],
                read: None,
            },
        ];
        for row in rows {
            let arguments: Vec<String> =
                row.arguments.iter().map(|word| (*word).to_string()).collect();
            check_reading(row.case, &arguments, row.read.as_ref());
        }
        // One more name than an invocation performs, in either spelling: the cap is the same.
        for (case, arguments) in [
            ("more bare names than one invocation performs", &past_the_cap),
            (
                "more names than one invocation performs, as options",
                &past_the_cap_as_options,
            ),
        ] {
            check_reading(case, arguments, None);
        }
    }

    /// Read `arguments` and assert they came to `expected`.
    fn check_reading(case: &str, arguments: &[String], expected: Option<&Read>) {
        let read = Invocation::read(arguments);
        match (read.as_ref(), expected) {
            (Some(read), Some(wanted)) => {
                assert_eq!(
                    read.server.as_deref(),
                    wanted.server,
                    "case {case:?}: expected server {:?}, got {:?}",
                    wanted.server,
                    read.server
                );
                assert_eq!(
                    read.names, wanted.names,
                    "case {case:?}: expected names {:?}, got {:?}",
                    wanted.names, read.names
                );
                assert_eq!(
                    read.record_type, wanted.record_type,
                    "case {case:?}: expected type {:?}, got {:?}",
                    wanted.record_type, read.record_type
                );
                assert_eq!(
                    (read.short, read.over_tcp),
                    (wanted.short, wanted.over_tcp),
                    "case {case:?}: expected the forms {:?}, got {:?}",
                    (wanted.short, wanted.over_tcp),
                    (read.short, read.over_tcp)
                );
            }
            (None, None) => {}
            (read, expected) => panic!(
                "case {case:?}: expected {expected:?}, got {:?}",
                read.map(|read| read.names.clone())
            ),
        }
    }

    /// One row of the lookup table: the arguments, and the type and protocol the lookup reads
    /// out of them — or `None` for a form it performs nothing of.
    struct LookupRow {
        case: &'static str,
        arguments: &'static [&'static str],
        read: Option<(&'static str, &'static str)>,
    }

    /// RULE: the lookup client READS its settings — the type asked for above all — and performs
    /// nothing of a form it does not read, rather than answering a different query from the one
    /// it was given.
    ///
    /// The rows span what the reading generalises over: each spelling of the type setting, the
    /// case it is written in, the connection form, a setting read and ignored, the class and the
    /// port it serves and the ones it does not, and a setting it has no reading for.
    #[test]
    fn the_lookup_clients_settings_are_read_not_passed_over() {
        let rows = [
            LookupRow {
                case: "a name alone",
                arguments: &["app.example.internal"],
                read: Some(("A", PROTOCOL_UDP)),
            },
            LookupRow {
                case: "the type setting",
                arguments: &["-type=CNAME", "www.example.internal"],
                read: Some(("CNAME", PROTOCOL_UDP)),
            },
            LookupRow {
                case: "its querytype spelling, lower case",
                arguments: &["-querytype=txt", "txt.example.internal"],
                read: Some(("TXT", PROTOCOL_UDP)),
            },
            LookupRow {
                case: "its short spelling, with a server",
                arguments: &["-q=AAAA", "ipv6.example.internal", "10.91.1.10"],
                read: Some(("AAAA", PROTOCOL_UDP)),
            },
            LookupRow {
                case: "the connection form",
                arguments: &["-vc", "app.example.internal"],
                read: Some(("A", PROTOCOL_TCP)),
            },
            LookupRow {
                case: "a setting about the transport",
                arguments: &["-timeout=2", "-retry=1", "app.example.internal"],
                read: Some(("A", PROTOCOL_UDP)),
            },
            LookupRow {
                case: "the class and the port it serves",
                arguments: &["-class=IN", "-port=53", "app.example.internal"],
                read: Some(("A", PROTOCOL_UDP)),
            },
            LookupRow {
                case: "a class it does not serve",
                arguments: &["-class=CH", "app.example.internal"],
                read: None,
            },
            LookupRow {
                case: "a port it does not serve",
                arguments: &["-port=5353", "app.example.internal"],
                read: None,
            },
            LookupRow {
                case: "a type no record of this data plane has",
                arguments: &["-type=NOTATYPE", "app.example.internal"],
                read: None,
            },
            LookupRow {
                case: "a setting it has no reading for",
                arguments: &["-norecurse", "app.example.internal"],
                read: None,
            },
            LookupRow {
                case: "no name at all",
                arguments: &["-type=A"],
                read: None,
            },
        ];
        for row in rows {
            let arguments: Vec<String> =
                row.arguments.iter().map(|word| (*word).to_string()).collect();
            let read = Lookup::read(&arguments);
            let actual = read
                .as_ref()
                .map(|lookup| (lookup.record_type.clone(), lookup.protocol()));
            let wanted = row
                .read
                .map(|(record_type, protocol)| (record_type.to_string(), protocol));
            assert_eq!(
                actual, wanted,
                "case {:?}: expected {wanted:?}, got {actual:?}",
                row.case
            );
        }
    }
}
