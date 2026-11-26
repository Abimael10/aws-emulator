// SPDX-License-Identifier: CC0-1.0

//! The SLOWLOG family a cache's engine SERVES, and what each of its subcommands answers.
//!
//! The rule under test: the engine a cache serves on its endpoint answers the administrative
//! `SLOWLOG` family out of the slow log of the NODE the session is on — `GET [<count>]` with
//! the recorded entries, newest first, [`DEFAULT_PAGE`] of them for a client that names no
//! count and every one the log holds for `-1`; `LEN` with a RESP integer counting what it
//! holds; `RESET` with a status, after which nothing recorded before it is reported while the
//! entry ids go on counting; and `HELP` with the family's own usage. Only a subcommand the
//! family does not have is refused — never the verb itself, which is a command of the engine
//! — and a refusal quotes the subcommand the client WROTE.
//!
//! The rows span what the rule generalises over: which subcommand is sent, which COUNT form
//! `GET` is given (none, a count, `0`, `-1`, one below `-1`, one that is not a number), the
//! CASE the client wrote the verb and the subcommand in, and WHICH node of a cache the
//! session is on — a standalone cluster's, and each member of a replication group, whose
//! members share one keyspace and keep a slow log each.
//!
//! What the sibling suite [`crate::cache_slow_log`] pins is which commands are recorded and
//! how many are kept, over the thresholds and depths a parameter group states; this one is
//! the command surface that reads them. Every assertion goes over a real socket the core
//! opened for the cache, dialled at the address the control plane published.

use crate::cache_data_plane::{command, endpoint_of, open, Resp, Session};
use crate::cache_slow_log::{recording_parameter_group, session_on, EVERY_COMMAND};
use crate::harness::{params, poll_until, Emulator, REGION};

/// How many entries `SLOWLOG GET` answers with when the client names no count: the engine's
/// own default, as the family's own `HELP` states it ("default: 10, -1 mean all").
const DEFAULT_PAGE: usize = 10;

/// How many entries the caches below keep, which is more than any row records.
const DEPTH: &str = "128";

/// How many commands are sent before the rows read the log, so that more than one page of
/// entries is held and a page can be told from the whole log.
const SEEDED: usize = DEFAULT_PAGE + 2;

/// The refusal a `GET` whose count is neither `-1` nor a count carries — the engine's own
/// range message for that argument.
const BAD_COUNT: &str = "ERR count should be greater than or equal to -1";

/// What a row expects of the reply its command is answered with.
#[derive(Debug)]
enum Expected {
    /// A RESP integer reply, of whatever value: the log's own length grows with the very
    /// commands these rows send, so what is pinned here is the reply TYPE, and the counting
    /// itself is asserted against the entries read back below.
    AnyInteger,
    /// An array holding exactly this many entries.
    Entries(usize),
    /// Exactly this error.
    Refusal(&'static str),
    /// An array of lines, together naming each of these words.
    LinesNaming(&'static [&'static str]),
}

/// The entries `SLOWLOG GET <count>` reports, each as its id and the arguments of the command
/// it recorded.
///
/// Richer than the reader of the sibling suite, which needs the arguments alone: the rows
/// below assert on the ids as well, because an entry's id is what makes it one entry of this
/// node's log rather than of what the log happens to hold now.
async fn entries_of(case: &str, session: &mut Session, count: &str) -> Vec<(i64, Vec<String>)> {
    let answered = command(case, session, &["SLOWLOG", "GET", count]).await;
    let Resp::Array(entries) = answered else {
        panic!("case {case}: SLOWLOG GET {count} must answer an array, got {answered:?}");
    };
    entries.into_iter().map(|entry| entry_of(case, entry)).collect()
}

/// One entry of that answer: its id and the command's arguments.
fn entry_of(case: &str, entry: Resp) -> (i64, Vec<String>) {
    let Resp::Array(fields) = entry else {
        panic!("case {case}: a slow-log entry must be an array, got {entry:?}");
    };
    assert_eq!(
        fields.len(),
        6,
        "case {case}: an entry carries its id, instant, duration, arguments, client address \
         and client name, got {fields:?}"
    );
    let Resp::Integer(id) = fields[0] else {
        panic!(
            "case {case}: an entry's id must be a number, got {:?}",
            fields[0]
        );
    };
    let Resp::Array(ref arguments) = fields[3] else {
        panic!(
            "case {case}: an entry's arguments must be an array, got {:?}",
            fields[3]
        );
    };
    let arguments = arguments
        .iter()
        .map(|argument| match argument {
            Resp::Bulk(text) => text.clone(),
            other => panic!("case {case}: an argument must be a bulk string, got {other:?}"),
        })
        .collect();
    (id, arguments)
}

/// Send [`SEEDED`] commands the cache records, so the log holds more than one page.
async fn seed(case: &str, session: &mut Session) {
    for index in 1..=SEEDED {
        let key = format!("seeded-{index}");
        assert_eq!(
            command(case, session, &["SET", &key, "recorded"]).await,
            Resp::Status("OK".to_string()),
            "case {case}: the cache must take the write of {key}"
        );
    }
}

/// Whether the entries read back carry a command whose first argument is `verb`.
fn names_verb(entries: &[(i64, Vec<String>)], verb: &str) -> bool {
    entries
        .iter()
        .any(|(_id, arguments)| arguments.first().is_some_and(|first| first == verb))
}

#[tokio::test]
async fn the_slowlog_family_answers_the_reply_each_subcommand_documents() {
    let emulator = Emulator::new();
    let case = "slowlog-surface";
    let mut session = session_on(&emulator, case, EVERY_COMMAND, DEPTH).await;
    seed(case, &mut session).await;
    // The axis: the subcommand sent, and the reply the engine answers it with. `RESET` is
    // sent after the table, because it discards what the rows above read.
    let rows: &[(&str, &[&str], Expected)] = &[
        ("the length of the log", &["SLOWLOG", "LEN"], Expected::AnyInteger),
        (
            "a page the client named no count for",
            &["SLOWLOG", "GET"],
            Expected::Entries(DEFAULT_PAGE),
        ),
        ("a page of three", &["SLOWLOG", "GET", "3"], Expected::Entries(3)),
        ("a page of none", &["SLOWLOG", "GET", "0"], Expected::Entries(0)),
        (
            "the verb and its subcommand as the client wrote them",
            &["slowlog", "len"],
            Expected::AnyInteger,
        ),
        (
            "the family's own usage",
            &["SLOWLOG", "HELP"],
            Expected::LinesNaming(&["GET", "LEN", "RESET", "HELP"]),
        ),
        (
            "a count below the one that means every entry",
            &["SLOWLOG", "GET", "-2"],
            Expected::Refusal(BAD_COUNT),
        ),
        (
            "a count that is not a number",
            &["SLOWLOG", "GET", "ten"],
            Expected::Refusal(BAD_COUNT),
        ),
        (
            "a subcommand the family does not have",
            &["SLOWLOG", "BoGuS"],
            Expected::Refusal(
                "ERR Unknown subcommand or wrong number of arguments for 'BoGuS'. Try SLOWLOG \
                 HELP.",
            ),
        ),
        (
            "the verb with no subcommand",
            &["SLOWLOG"],
            Expected::Refusal("ERR wrong number of arguments for 'slowlog' command"),
        ),
    ];
    for (what, sent, expected) in rows {
        let answered = command(case, &mut session, sent).await;
        match expected {
            Expected::AnyInteger => assert!(
                matches!(answered, Resp::Integer(count) if count >= 0),
                "case {case}/{what}: {sent:?} must answer a RESP integer, got {answered:?}"
            ),
            Expected::Entries(held) => {
                let Resp::Array(ref entries) = answered else {
                    panic!(
                        "case {case}/{what}: {sent:?} must answer an array, got {answered:?}"
                    );
                };
                assert_eq!(
                    entries.len(),
                    *held,
                    "case {case}/{what}: {sent:?} must answer {held} entries, got {answered:?}"
                );
                for entry in entries {
                    let _parsed = entry_of(case, entry.clone());
                }
            }
            Expected::Refusal(message) => assert_eq!(
                answered,
                Resp::Error((*message).to_string()),
                "case {case}/{what}: {sent:?} must be refused {message:?}, got {answered:?}"
            ),
            Expected::LinesNaming(words) => {
                let Resp::Array(ref lines) = answered else {
                    panic!(
                        "case {case}/{what}: {sent:?} must answer an array, got {answered:?}"
                    );
                };
                let text = lines
                    .iter()
                    .map(|line| match line {
                        Resp::Bulk(line) | Resp::Status(line) => line.clone(),
                        other => panic!(
                            "case {case}/{what}: a usage line must be text, got {other:?}"
                        ),
                    })
                    .collect::<Vec<String>>()
                    .join("\n");
                for word in *words {
                    assert!(
                        text.contains(word),
                        "case {case}/{what}: the usage must name {word}, got {text:?}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_count_of_minus_one_answers_every_entry_the_log_holds() {
    let emulator = Emulator::new();
    let case = "slowlog-every-entry";
    let mut session = session_on(&emulator, case, EVERY_COMMAND, DEPTH).await;
    seed(case, &mut session).await;
    // What `LEN` counts is what `-1` answers with: the count read first can only be lower,
    // because reading it is itself a command this cache records.
    let counted = command(case, &mut session, &["SLOWLOG", "LEN"]).await;
    let Resp::Integer(counted) = counted else {
        panic!("case {case}: SLOWLOG LEN must answer a RESP integer, got {counted:?}");
    };
    assert!(
        counted >= i64::try_from(SEEDED).unwrap_or(i64::MAX),
        "case {case}: the log must count the {SEEDED} commands it recorded, got {counted}"
    );
    let all = entries_of(case, &mut session, "-1").await;
    assert!(
        all.len() >= usize::try_from(counted).unwrap_or(usize::MAX),
        "case {case}: a count of -1 answers every entry the log holds, got {} for a log of \
         {counted}",
        all.len()
    );
    assert!(
        all.len() > DEFAULT_PAGE,
        "case {case}: a count of -1 is not held to the default page of {DEFAULT_PAGE}, got {}",
        all.len()
    );
    // Newest first, so the last command recorded before the read heads the answer.
    assert_eq!(
        all.first().map(|(_id, arguments)| arguments.first().cloned()),
        Some(Some("SLOWLOG".to_string())),
        "case {case}: the entries are answered newest first, got {all:?}"
    );
}

#[tokio::test]
async fn a_reset_discards_the_entries_held_and_the_ids_go_on_counting() {
    let emulator = Emulator::new();
    let case = "slowlog-reset";
    let mut session = session_on(&emulator, case, EVERY_COMMAND, DEPTH).await;
    seed(case, &mut session).await;
    let before = entries_of(case, &mut session, "-1").await;
    assert!(
        names_verb(&before, "SET"),
        "case {case}: the writes sent must be recorded before the reset, got {before:?}"
    );
    let highest = before
        .iter()
        .map(|(id, _arguments)| *id)
        .max()
        .unwrap_or_default();
    assert_eq!(
        command(case, &mut session, &["SLOWLOG", "RESET"]).await,
        Resp::Status("OK".to_string()),
        "case {case}: SLOWLOG RESET must be served"
    );
    assert_eq!(
        command(case, &mut session, &["GET", "seeded-1"]).await,
        Resp::Bulk("recorded".to_string()),
        "case {case}: the cache must still answer its keyspace after a reset"
    );
    let after = entries_of(case, &mut session, "-1").await;
    assert!(
        !names_verb(&after, "SET"),
        "case {case}: no entry recorded before the reset may be reported, got {after:?}"
    );
    assert!(
        names_verb(&after, "GET"),
        "case {case}: the commands after the reset are recorded, got {after:?}"
    );
    assert!(
        after
            .iter()
            .all(|(id, _arguments)| *id > highest),
        "case {case}: an entry's id is unique for the node, so the ids go on counting past \
         {highest}, got {after:?}"
    );
}

#[tokio::test]
async fn every_member_of_a_group_answers_the_family_out_of_its_own_nodes_log() {
    let emulator = Emulator::new();
    let case = "slowlog-per-node";
    let group = recording_parameter_group(&emulator, case, EVERY_COMMAND, DEPTH).await;
    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", case),
                ("ReplicationGroupDescription", "a group with a replica"),
                ("Engine", "redis"),
                ("NumCacheClusters", "2"),
                ("AutomaticFailoverEnabled", "true"),
                ("CacheParameterGroupName", &group),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case}: the group must be created, got {} {}",
        created.status, created.body
    );
    let described = poll_until(
        &emulator,
        REGION,
        case,
        "the group reaching available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", case)]),
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
    )
    .await;
    let (primary_address, primary_port) = endpoint_of(
        &described,
        "NodeGroup/PrimaryEndpoint/Address",
        "NodeGroup/PrimaryEndpoint/Port",
        case,
    );
    let (reader_address, reader_port) = endpoint_of(
        &described,
        "NodeGroup/ReaderEndpoint/Address",
        "NodeGroup/ReaderEndpoint/Port",
        case,
    );
    let mut primary = open("the primary", &primary_address, &primary_port).await;
    let mut reader = open("the replica", &reader_address, &reader_port).await;
    assert_eq!(
        command("the primary", &mut primary, &["SET", "shared", "written-on-the-primary"]).await,
        Resp::Status("OK".to_string()),
        "case {case}: the primary must take the write"
    );
    assert_eq!(
        command("the replica", &mut reader, &["GET", "shared"]).await,
        Resp::Bulk("written-on-the-primary".to_string()),
        "case {case}: the members share one keyspace, so the replica answers that write"
    );
    // The family is served on EITHER member: reading a slow log is no write, so a replica
    // answers it too.
    for (member, session) in [
        ("the primary", &mut primary),
        ("the replica", &mut reader),
    ] {
        let counted = command(member, session, &["SLOWLOG", "LEN"]).await;
        assert!(
            matches!(counted, Resp::Integer(count) if count > 0),
            "case {case}/{member}: SLOWLOG LEN must count what this node executed, got \
             {counted:?}"
        );
    }
    // And each answers out of the log of what ITS OWN node executed.
    let on_primary = entries_of("the primary", &mut primary, "-1").await;
    let on_replica = entries_of("the replica", &mut reader, "-1").await;
    assert!(
        names_verb(&on_primary, "SET"),
        "case {case}: the node that executed the write records it, got {on_primary:?}"
    );
    assert!(
        !names_verb(&on_replica, "SET"),
        "case {case}: a node's slow log is the log of what IT executed, so the write served \
         on the primary is not in the replica's, got {on_replica:?}"
    );
    assert!(
        names_verb(&on_replica, "GET"),
        "case {case}: the read served on the replica is recorded there, got {on_replica:?}"
    );
}
