// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the guest a MicroVM runs when its image's code artifact holds a
//! PYTHON program: that the published endpoint answers with THAT program's own response,
//! and that the program is ONE process whose memory, whose disk and whose published
//! identity survive its requests and a `SuspendMicrovm` + `ResumeMicrovm`.
//!
//! The suite beside this one (`microvm_guest_process`) pins the same rule for a guest the
//! JavaScript engine runs, and this one drives its harness — the image, the MicroVM, the
//! token and the endpoint — over the other engine: nothing of the rule is about the
//! language the image's command starts, so it is exercised here over every form the image
//! is staged in and over several values the guest keeps.
//!
//! Each test states a RULE and exercises it over a SET of inputs; the keys, values and ids
//! any one row carries are examples, not the specification. The control plane is driven
//! over the wire through the router the emulator boots, with Amazon S3 mounted beside this
//! service so the guest's code artifact is staged the way a client stages one; the
//! endpoint itself is driven through `state::microvm_endpoint::serve_request`, the one
//! decision the core's HTTPS listener hands every request on that address to.
//!
//! The guest's program is written here and packaged into that artifact: it keeps a value
//! in memory and the same value on disk, reports the identity its machine published to it,
//! and answers nothing this emulator chose.

use serde_json::json;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_endpoint::NO_GUEST_RESPONSE_STATUS;

use crate::microvm_guest_data_plane::{
    emulator, python_entry_artifact, python_specification, specification_only_artifact,
    stage_artifact,
};
use crate::microvm_guest_process::{
    auth_token, guest_answer, guest_image, microvm_in_state, running_microvm, through_endpoint,
    MARKER_VARIABLE,
};
use crate::microvm_lifecycle::call;
use crate::microvm_static_guest_data_plane::artifact;
use smithy_server_runtime::MultiServiceRouter;

/// The value the image declares [`MARKER_VARIABLE`] with here — the guest reports it back,
/// so the test reads the environment the image published rather than one this emulator
/// chose.
const MARKER_VALUE: &str = "a-python-published-value";

/// The PYTHON guest this suite runs: an `http.server` program that mints an id when its
/// own process starts, counts the requests it served, keeps a value in memory and the same
/// value on disk, and reports the identity its MicroVM published to it — this boot's id,
/// its process id, when its process started (field 22 of `/proc/self/stat`), its host name
/// and the environment variable its image declared ([`MARKER_VARIABLE`]).
const PYTHON_GUEST: &str = r#"
import json
import os
import socket
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

# Minted by the GUEST, once, when its process starts: two reads that report the same value
# are two reads of one process.
PROCESS_BOOT_ID = os.urandom(12).hex()

# What that one process holds between requests.
SERVED = {"count": 0}
IN_MEMORY = {}

# Where a value the guest is asked to keep is written on its machine's disk.
DISK_DIRECTORY = "/data"


def identity():
    """The identity the MicroVM published to this process."""
    with open("/proc/self/stat") as held:
        fields = held.read().split(" ")
    with open("/proc/sys/kernel/random/boot_id") as held:
        boot_id = held.read().strip()
    return {
        "processBootId": PROCESS_BOOT_ID,
        "machineBootId": boot_id,
        "pid": os.getpid(),
        "starttime": int(fields[21]),
        "hostname": socket.gethostname(),
        "marker": os.environ.get("SUITE_MARKER"),
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        key = query.get("key", [""])[0]
        if parsed.path == "/id":
            self.answer(200, identity())
            return
        if parsed.path == "/count":
            SERVED["count"] = SERVED["count"] + 1
            self.answer(200, {"count": SERVED["count"]})
            return
        if parsed.path == "/set":
            value = query.get("value", [""])[0]
            IN_MEMORY[key] = value
            with open(os.path.join(DISK_DIRECTORY, key), "w") as held:
                held.write(value)
            self.answer(200, {"stored": key})
            return
        if parsed.path == "/get":
            path = os.path.join(DISK_DIRECTORY, key)
            disk = None
            if os.path.exists(path):
                with open(path) as held:
                    disk = held.read()
            self.answer(200, {"memory": IN_MEMORY.get(key), "disk": disk})
            return
        self.answer(404, {"error": "no such path"})

    def answer(self, status, body):
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
"#;

/// One `POST` on a MicroVM's own lifecycle path (`suspend`, `resume`), which must be
/// accepted.
async fn lifecycle_call(router: &MultiServiceRouter, case: &str, microvm_id: &str, action: &str) {
    let (status, answered) = call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_id}/{action}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} the MicroVM {microvm_id}: expected 200, got {status} \
         {answered}"
    );
}

/// RULE: a MicroVM whose image's code artifact holds a PYTHON program runs that program as
/// its guest exactly as it runs a program of any other language the emulator's engines
/// cover, so an authenticated request to the published endpoint is answered by THAT
/// program — one process, whose values in memory, whose files on its machine's disk and
/// whose published identity are the same at every request and survive a `SuspendMicrovm`
/// followed by a `ResumeMicrovm`, while the suspended MicroVM itself has no guest to answer
/// a request, and another MicroVM of the same image is another machine running another
/// process that holds none of the first's memory, disk or identity.
///
/// The rows differ along the form the image is staged in — a guest a build step wrote, a
/// guest the archive carries as the entry the container command names, the single module of
/// a one-module archive, and the module a bare object is — and, within each, along the
/// values the guest is asked to keep (a plain one, one the query encodes a space in, a
/// replacement of a key already held) and the point in the MicroVM's life each is read
/// back at.
#[tokio::test(flavor = "multi_thread")]
async fn a_python_guest_is_one_process_whose_memory_disk_and_identity_survive_a_suspend() {
    // (the case of this staging form, the bucket it is staged in, the object key it is
    //  staged under, the artifact staged)
    let forms: Vec<(&str, &str, &str, Vec<u8>)> = vec![
        (
            "a Python guest a build step wrote",
            "mvm-python-process-built",
            "guest.zip",
            specification_only_artifact(&python_specification(PYTHON_GUEST)),
        ),
        (
            "a Python guest the archive carries as an entry",
            "mvm-python-process-entry",
            "guest.zip",
            python_entry_artifact(PYTHON_GUEST),
        ),
        (
            "the single module of a one-module archive",
            "mvm-python-process-single",
            "guest.zip",
            artifact(&[("app.py", PYTHON_GUEST)]),
        ),
        (
            "a bare object that is the guest's own module",
            "mvm-python-process-bare",
            "app.py",
            PYTHON_GUEST.as_bytes().to_vec(),
        ),
    ];
    // (the key, the value as the request's query carries it)
    let kept: Vec<(&str, &str)> = vec![
        ("alpha", "first-value"),
        ("beta", "second%20value"),
        ("alpha", "replaced"),
    ];
    // What the guest holds for each key once every row above has been written: the LAST
    // value written for it, decoded as the query carried it.
    let held: Vec<(&str, &str)> = vec![("alpha", "replaced"), ("beta", "second value")];

    for (case, bucket, key, staged) in forms {
        let (router, engine) = emulator();
        stage_artifact(&router, bucket, key, staged).await;
        let image = guest_image(
            &router,
            "python-guest-process",
            &format!("s3://{bucket}/{key}"),
            json!({MARKER_VARIABLE: MARKER_VALUE}),
        )
        .await;
        let (first, endpoint) = running_microvm(&router, &image).await;
        let (second, _second_endpoint) = running_microvm(&router, &image).await;
        assert!(
            !endpoint.is_empty(),
            "case {case:?}: a run MicroVM publishes an endpoint to reach its guest on"
        );
        let token = auth_token(&router, &first, json!([{"allPorts": {}}])).await;
        let other_token = auth_token(&router, &second, json!([{"allPorts": {}}])).await;

        // The identity the MicroVM published to its Python guest, read twice: one process
        // answers the same identity however often it is asked.
        let identity = guest_answer(&router, &engine, &first, "/id", &token).await;
        let again = guest_answer(&router, &engine, &first, "/id", &token).await;
        assert_eq!(
            identity, again,
            "case {case:?}: two reads of one running MicroVM must report one identity, got \
             {identity} then {again}"
        );
        assert_eq!(
            identity["hostname"].as_str(),
            Some(first.as_str()),
            "case {case:?}: the machine's host name is the MicroVM's own id, got {identity}"
        );
        assert_eq!(
            identity["marker"].as_str(),
            Some(MARKER_VALUE),
            "case {case:?}: the guest reads the environment its image declared, got {identity}"
        );
        let boot_id = identity["machineBootId"].as_str().unwrap_or_default();
        assert!(
            boot_id.len() == 36 && boot_id.split('-').count() == 5,
            "case {case:?}: the kernel publishes this boot's id as a UUID, got {boot_id:?}"
        );
        assert!(
            identity["pid"].as_i64().unwrap_or_default() > 0,
            "case {case:?}: the guest's process has a pid of its own, got {identity}"
        );
        assert!(
            identity["starttime"].as_i64().is_some(),
            "case {case:?}: the guest's process reports when it started, as the integer \
             field 22 of its own /proc/self/stat, got {identity}"
        );

        // The memory of that one process: each request reaches what the request before it
        // left behind.
        for expected in 1..=3 {
            let counted = guest_answer(&router, &engine, &first, "/count", &token).await;
            assert_eq!(
                counted["count"].as_i64(),
                Some(expected),
                "case {case:?}: request {expected} must reach the same process's counter, got \
                 {counted}"
            );
        }
        for (key, carried) in &kept {
            let stored = guest_answer(
                &router,
                &engine,
                &first,
                &format!("/set?key={key}&value={carried}"),
                &token,
            )
            .await;
            assert_eq!(
                stored["stored"].as_str(),
                Some(*key),
                "case {case:?}: the guest reports the key {key} it stored, got {stored}"
            );
        }
        for (key, expected) in &held {
            let read =
                guest_answer(&router, &engine, &first, &format!("/get?key={key}"), &token).await;
            assert_eq!(
                (read["memory"].as_str(), read["disk"].as_str()),
                (Some(*expected), Some(*expected)),
                "case {case:?}: the value {key} must be held in memory and on disk by the \
                 process that wrote it, got {read}"
            );
        }

        // A SUSPENDED MicroVM is not executing its guest, so no path of it is answered by
        // the guest.
        lifecycle_call(&router, case, &first, "suspend").await;
        microvm_in_state(&router, &first, "SUSPENDED").await;
        for path in ["/id", "/count", "/get?key=alpha"] {
            let (status, body) =
                through_endpoint(&router, &engine, &first, path, &token, None).await;
            assert_eq!(
                status, NO_GUEST_RESPONSE_STATUS,
                "case {case:?}: GET {path} on a SUSPENDED MicroVM must not be answered by \
                 its guest, got {status} {body}"
            );
        }

        // Resumed, it is the SAME process: the identity, the counter and every value it
        // held are the ones from before the suspend.
        lifecycle_call(&router, case, &first, "resume").await;
        microvm_in_state(&router, &first, "RUNNING").await;
        let after = guest_answer(&router, &engine, &first, "/id", &token).await;
        assert_eq!(
            after, identity,
            "case {case:?}: a resumed MicroVM runs the process it suspended, so its identity \
             is unchanged: expected {identity}, got {after}"
        );
        let counted = guest_answer(&router, &engine, &first, "/count", &token).await;
        assert_eq!(
            counted["count"].as_i64(),
            Some(4),
            "case {case:?}: the counter the process held through the suspend must carry on, \
             got {counted}"
        );
        for (key, expected) in &held {
            let read =
                guest_answer(&router, &engine, &first, &format!("/get?key={key}"), &token).await;
            assert_eq!(
                (read["memory"].as_str(), read["disk"].as_str()),
                (Some(*expected), Some(*expected)),
                "case {case:?}: the value {key} must survive the suspend in memory and on \
                 disk, got {read}"
            );
        }

        // The other MicroVM of the same image is another machine running another process.
        let other = guest_answer(&router, &engine, &second, "/id", &other_token).await;
        assert_eq!(
            other["hostname"].as_str(),
            Some(second.as_str()),
            "case {case:?}: the second MicroVM's guest reports its own host name, got {other}"
        );
        assert_ne!(
            other["machineBootId"], identity["machineBootId"],
            "case {case:?}: two MicroVMs are two boots, so their boot ids differ: got {other} \
             and {identity}"
        );
        assert_ne!(
            other["processBootId"], identity["processBootId"],
            "case {case:?}: two MicroVMs run two processes, so the id each minted differs: \
             got {other} and {identity}"
        );
        let counted = guest_answer(&router, &engine, &second, "/count", &other_token).await;
        assert_eq!(
            counted["count"].as_i64(),
            Some(1),
            "case {case:?}: the second MicroVM's process holds none of the first's memory, \
             got {counted}"
        );
        let read =
            guest_answer(&router, &engine, &second, "/get?key=alpha", &other_token).await;
        assert_eq!(
            (read["memory"].as_str(), read["disk"].as_str()),
            (None, None),
            "case {case:?}: the second MicroVM's machine holds none of the first's disk, got \
             {read}"
        );
    }
}
