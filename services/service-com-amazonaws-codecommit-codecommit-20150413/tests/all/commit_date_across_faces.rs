// SPDX-License-Identifier: CC0-1.0

//! WHICH DATE a commit carries on EVERY face of the service that reports that
//! commit, driven over the wire through the router that serves the real emulator.
//!
//! The rule under test: a commit's author and committer date is part of the commit
//! OBJECT — "The date when the specified commit was commited, in timestamp format
//! with GMT offset" (`smithy_query(query="doc",shape="UserInfo",member="date")`) —
//! so it is established once, when that object is written, and every face that
//! reports the commit reports that same date: `GetCommit`, the `revisionDag`
//! `ListFileCommitHistory` serves, and the commit object the git transport hands a
//! client, whose SHA-1 name is the `commitId` all of them report. Nothing re-derives
//! it per read, and nothing a later write does moves it.
//!
//! The decisive oracle is a commit a git client PUSHES: its identity lines carry
//! dates the CLIENT chose, nowhere near the instant the emulator serves a read at,
//! so a reader that filled the date from its own clock — or from anything but the
//! stored object — cannot agree with them. That is the half of the rule the sibling
//! suite [`crate::commit_date_identity`] cannot state, because every commit it
//! writes is stamped by the service itself.
//!
//! The table's rows differ along what the rule generalises over: whether the author
//! and the committer were stamped at one instant or two, the GMT offset the date
//! carries (`+0000`, a whole-hour negative one and a half-hour positive one), and
//! where the instant sits — the epoch, the past, and beyond any clock the emulator
//! could read. The last test covers the other writer, a commit the service stamps
//! itself, and holds all three faces to one date for it too.
//!
//! The git wire builders (the commit, tree and blob objects, the packfile, the pkt-lines,
//! a push body and a fetch body) are the ones [`crate::git_transport`] owns, and the
//! control-plane harness and the two API readers of a commit are the shared ones in
//! [`crate::repository_lifecycle`] and [`crate::commit_date_identity`]: this suite adds no
//! second copy of any of them.

use std::collections::BTreeMap;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::git_transport::{
    command, commit_with_identities, fetch_body, id_of, parse_fetch, parse_pkts, push_body, rpc,
    snapshot_objects, unpack, Object, NULL_ID,
};
// The two API readers of a commit are asked for it through the sibling suite's
// helpers, so this target holds ONE definition of "the commit GetCommit reports"
// and "the revision DAG of a path".
use crate::commit_date_identity::{fetched, git_date_seconds, history};
use crate::repository_lifecycle::{base64, emulator, ok, string_at};

/// The branch every case pushes to and writes on.
const BRANCH: &str = "main";

/// The file whose history every case reads back.
const PATH: &str = "docs/sub/notes.txt";

/// The git tree entry mode of a regular file.
const REGULAR_FILE: &str = "100644";

/// The identity the pushed commits carry. The author and the committer are
/// DIFFERENT people, so a reader that reported one of them for both would be caught
/// as well.
const AUTHOR: (&str, &str) = ("Pushing Author", "author@example.com");
const COMMITTER: (&str, &str) = ("Pushing Committer", "committer@example.com");

/// One pushed commit: the dates its two identity lines carry.
#[derive(Debug, Clone, Copy)]
struct Row {
    label: &'static str,
    author_date: &'static str,
    committer_date: &'static str,
}

/// The table. Rows differ along the number of instants (one, two), the GMT offset
/// (`+0000`, `-0700`, `+0530`) and where the instant sits relative to any clock the
/// emulator could read (the epoch, the past, the far future).
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "one instant at GMT",
            author_date: "1770000000 +0000",
            committer_date: "1770000000 +0000",
        },
        Row {
            label: "two instants, a negative whole-hour offset",
            author_date: "1500000000 -0700",
            committer_date: "1500003600 -0700",
        },
        Row {
            label: "a half-hour offset",
            author_date: "1234567890 +0530",
            committer_date: "1234567890 +0530",
        },
        Row {
            label: "the epoch",
            author_date: "0 +0000",
            committer_date: "0 +0000",
        },
        Row {
            label: "beyond any clock this emulator can read",
            author_date: "4102444800 +0000",
            committer_date: "4102444801 +0000",
        },
    ]
}

/// A commit object as a git client builds it, with the dates of a row: the git commit
/// encoding is the one the sibling transport suite owns, handed the two identity lines this
/// suite is about.
fn commit_object(
    tree_id: &str,
    parents: &[String],
    message: &str,
    author_date: &str,
    committer_date: &str,
) -> Object {
    commit_with_identities(
        tree_id,
        parents,
        message,
        &format!("{} <{}> {author_date}", AUTHOR.0, AUTHOR.1),
        &format!("{} <{}> {committer_date}", COMMITTER.0, COMMITTER.1),
    )
}

/// The date on the `role` identity line of a commit object: what git writes after
/// the email, which is `<seconds> <±hhmm>`.
fn identity_date(data: &[u8], role: &str) -> String {
    let text = String::from_utf8_lossy(data).into_owned();
    let line = text
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix(&format!("{role} ")))
        .unwrap_or_else(|| panic!("a commit object carries a {role} line, got {text:?}"))
        .to_string();
    line.split_once("> ")
        .unwrap_or_else(|| panic!("the {role} line names an email, got {line:?}"))
        .1
        .trim()
        .to_string()
}

/// Create a repository, answering the clone URL the API announces for it.
async fn repository(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(
        router,
        "CreateRepository",
        &json!({ "repositoryName": name }),
    )
    .await;
    string_at(&created, "/repositoryMetadata/cloneUrlHttp")
}

/// The commit object the git transport serves for `commit_id`.
async fn served_commit_object(
    router: &MultiServiceRouter,
    clone_url: &str,
    commit_id: &str,
) -> Object {
    let reply = rpc(
        router,
        clone_url,
        "git-upload-pack",
        fetch_body(&[commit_id.to_string()], &[]),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "a fetch of {commit_id} must be served, got status {} and body {:?}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
    let (_, packfile) = parse_fetch(&reply.body);
    unpack(&packfile)
        .into_iter()
        .find(|object| id_of(object) == commit_id)
        .unwrap_or_else(|| panic!("the pack served for {commit_id} must carry that commit"))
}

/// Write one file through the API, answering the commit id it produced.
async fn put_file(
    router: &MultiServiceRouter,
    name: &str,
    path: &str,
    content: &[u8],
    parent: Option<&str>,
) -> String {
    let mut input = json!({
        "repositoryName": name,
        "branchName": BRANCH,
        "filePath": path,
        "fileContent": base64(content),
        "commitMessage": format!("write {path}"),
        "name": AUTHOR.0,
        "email": AUTHOR.1,
    });
    if let Some(parent) = parent {
        input["parentCommitId"] = json!(parent);
    }
    string_at(&ok(router, "PutFile", &input).await, "/commitId")
}

#[tokio::test]
async fn a_pushed_commits_dates_are_the_ones_every_face_reports() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let label = row.label;
        let name = format!("pushed-commit-date-{index}");
        let clone_url = repository(&router, &name).await;

        // One file, one commit, with the row's identity dates — built here, so the
        // dates under test are the CLIENT's and the commit's name is computed from
        // them by this test rather than by the service.
        let files: BTreeMap<String, (String, Vec<u8>)> = [(
            PATH.to_string(),
            (
                REGULAR_FILE.to_string(),
                format!("the pushed version of {label}\n").into_bytes(),
            ),
        )]
        .into_iter()
        .collect();
        let (root, mut objects) = snapshot_objects(&files);
        let object = commit_object(
            &root,
            &[],
            "add the file",
            row.author_date,
            row.committer_date,
        );
        let commit_id = id_of(&object);
        let pushed_bytes = object.data.clone();
        objects.push(object);

        let reply = rpc(
            &router,
            &clone_url,
            "git-receive-pack",
            push_body(
                &[command(
                    NULL_ID,
                    &commit_id,
                    &format!("refs/heads/{BRANCH}"),
                    "report-status",
                )],
                &objects,
            ),
        )
        .await;
        let (report, _) = parse_pkts(&reply.body);
        assert_eq!(
            report,
            vec![
                "unpack ok\n".to_string(),
                format!("ok refs/heads/{BRANCH}\n")
            ],
            "case {label:?}: the push must be accepted for the dates to be under test"
        );

        // FACE ONE — the commit reader.
        let commit = fetched(&router, &name, &commit_id).await;
        assert_eq!(
            string_at(&commit, "/commitId"),
            commit_id,
            "case {label:?}: GetCommit must report the id the client computed, got {commit}"
        );
        assert_eq!(
            string_at(&commit, "/author/date"),
            row.author_date,
            "case {label:?}: the author date is the commit object's own {:?}, got {commit}",
            row.author_date
        );
        assert_eq!(
            string_at(&commit, "/committer/date"),
            row.committer_date,
            "case {label:?}: the committer date is the commit object's own {:?}, got {commit}",
            row.committer_date
        );

        // FACE TWO — the file history embeds the very same commit.
        let dag = history(&router, &name, PATH, &commit_id).await;
        assert_eq!(
            dag.len(),
            1,
            "case {label:?}: one commit introduced {PATH}, so it has one revision, got {dag:?}"
        );
        let embedded = dag[0]
            .get("commit")
            .cloned()
            .unwrap_or_else(|| panic!("case {label:?}: expected a commit in {}", dag[0]));
        assert_eq!(
            embedded, commit,
            "case {label:?}: the history's commit must be the commit GetCommit reports, got \
             {embedded} against {commit}"
        );

        // FACE THREE — the object the git transport serves is the one that was
        // pushed, so its identity lines carry the dates the API reported and its
        // name is the id both of them report.
        let served = served_commit_object(&router, &clone_url, &commit_id).await;
        assert_eq!(
            served.kind, "commit",
            "case {label:?}: {commit_id} is served as a commit object"
        );
        assert_eq!(
            served.data, pushed_bytes,
            "case {label:?}: the transport must serve the commit object as pushed, got {:?}",
            String::from_utf8_lossy(&served.data)
        );
        for (role, reported) in [
            ("author", string_at(&commit, "/author/date")),
            ("committer", string_at(&commit, "/committer/date")),
        ] {
            let in_object = identity_date(&served.data, role);
            assert_eq!(
                in_object, reported,
                "case {label:?}: the {role} date in the served object must be the one the API \
                 reports, got {in_object:?} against {reported:?}"
            );
        }

        // A LATER WRITE reads the clock again and must not move what was already
        // handed out: the commit is immutable.
        let later = put_file(
            &router,
            &name,
            "other.txt",
            b"a later write\n",
            Some(&commit_id),
        )
        .await;
        assert_ne!(
            later, commit_id,
            "case {label:?}: the later write must produce its own commit"
        );
        let after = fetched(&router, &name, &commit_id).await;
        assert_eq!(
            after, commit,
            "case {label:?}: a later write must leave {commit_id} reporting the same commit, got \
             {after} after {commit}"
        );
        let dag_after = history(&router, &name, PATH, &commit_id).await;
        assert_eq!(
            dag_after, dag,
            "case {label:?}: a write to another file must leave the history of {PATH} unchanged"
        );
    }
}

#[tokio::test]
async fn a_service_stamped_commits_dates_agree_on_every_face_too() {
    let router = emulator();
    let name = "stamped-commit-date";
    let clone_url = repository(&router, name).await;

    // Rows: the commit that introduced the file and the one that revised it, so a
    // root commit and a child are both read back through all three faces.
    let first = put_file(&router, name, PATH, b"one\n", None).await;
    let second = put_file(&router, name, PATH, b"two\n", Some(&first)).await;

    let dag = history(&router, name, PATH, &second).await;
    assert_eq!(
        dag.len(),
        2,
        "two writes of {PATH} are two revisions of it, got {dag:?}"
    );

    for (position, commit_id) in [(1usize, &first), (0usize, &second)] {
        let case = format!("revision at position {position}");
        let commit = fetched(&router, name, commit_id).await;
        let author = string_at(&commit, "/author/date");
        let committer = string_at(&commit, "/committer/date");
        assert!(
            git_date_seconds(&author).is_some() && git_date_seconds(&committer).is_some(),
            "case {case:?}: the dates must be <unix-seconds> <±hhmm>, got {author:?} and \
             {committer:?}"
        );
        assert_eq!(
            author, committer,
            "case {case:?}: one write stamps one instant, got author {author:?} and committer \
             {committer:?}"
        );

        let embedded = dag[position]
            .get("commit")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a commit in {}", dag[position]));
        assert_eq!(
            string_at(&embedded, "/commitId"),
            *commit_id,
            "case {case:?}: the history is newest first, so position {position} is {commit_id}"
        );
        assert_eq!(
            embedded, commit,
            "case {case:?}: the history's commit must be the commit GetCommit reports, got \
             {embedded} against {commit}"
        );

        let served = served_commit_object(&router, &clone_url, commit_id).await;
        for (role, reported) in [("author", &author), ("committer", &committer)] {
            let in_object = identity_date(&served.data, role);
            assert_eq!(
                &in_object, reported,
                "case {case:?}: the {role} date hashed into {commit_id} must be the one the API \
                 reports, got {in_object:?} against {reported:?}"
            );
        }
    }
}
