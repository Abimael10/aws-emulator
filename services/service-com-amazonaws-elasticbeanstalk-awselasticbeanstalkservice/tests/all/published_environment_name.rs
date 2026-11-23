// SPDX-License-Identifier: CC0-1.0

//! The NAME an environment publishes, over the service's own HTTP surface.
//!
//! The rule under test: an environment's published CNAME
//! (`EnvironmentDescription$CNAME`, "the URL to the CNAME for this environment")
//! is a DNS NAME — its first label is the environment's CNAME prefix, its second
//! its Region, and the rest is a domain — so a client recovers the prefix an
//! environment holds as the first dot-label of that value and can hand it
//! straight back to `CheckDNSAvailability`; the value carries no path and no
//! delimiter a name cannot hold, and it is an address this emulator ANSWERS at,
//! so a plain HTTP request for it is served by the application the environment
//! runs. Beside it, `EndpointURL` is the ADDRESS that answers behind the name
//! ("the URL to the LoadBalancer", "the IP address of the instance"), which is
//! this emulator's own socket. `SwapEnvironmentCNAMEs` exchanges the NAMES, which
//! exchanges which application answers at each of them, and leaves each
//! environment's own address where it was.
//!
//! The table spans what the rule generalises over: the Region the environment is
//! launched in, whether the launch supplies the CNAME prefix or leaves it to be
//! generated from the environment's own name, and the prefix's length at both
//! ends of the `DNSCnamePrefix` `@length(4, 63)` bound and in mixed case (a CNAME
//! prefix is a host name label, and DNS compares labels case-insensitively).
//!
//! The HTTP harness (the router, the form encoder, the XML element reader, the
//! object-storage upload and the unsigned GET) is the one `lifecycle.rs` owns in
//! this same test binary.

use crate::application_version_processing::zip_of;
use crate::harness::{wait_for_status, Answer, Emulator, SOLUTION_STACK};
use crate::lifecycle::TEST_REGION;

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every environment below is launched under.
const APPLICATION: &str = "published-name-app";

/// The bucket prefix the suite below uploads each source bundle into: one bucket
/// per bundle, because creating a bucket that already exists is refused.
const BUNDLE_BUCKET_PREFIX: &str = "eb-published-name-bundles";

/// One launch, and what its published name must then be read back as.
struct NameRow {
    case: &'static str,
    /// The Region the launch is signed for, which is the name's SECOND label.
    region: &'static str,
    environment_name: &'static str,
    /// The `CNAMEPrefix` the launch supplies, or `None` for a launch that leaves
    /// it to be generated ("if not specified, the CNAME is generated
    /// automatically by appending a random alphanumeric string to the
    /// environment name").
    cname_prefix: Option<String>,
}

/// The rows, one per form the rule generalises over.
fn rows() -> Vec<NameRow> {
    vec![
        NameRow {
            case: "a supplied prefix",
            region: TEST_REGION,
            environment_name: "named-env",
            cname_prefix: Some("published-name".to_string()),
        },
        NameRow {
            case: "a generated prefix",
            region: TEST_REGION,
            environment_name: "generated-env",
            cname_prefix: None,
        },
        NameRow {
            case: "another Region",
            region: "eu-central-1",
            environment_name: "other-region-env",
            // The same prefix in two Regions is two names, so the Region label is
            // what tells them apart.
            cname_prefix: Some("published-name".to_string()),
        },
        NameRow {
            case: "a prefix at the length trait's minimum",
            region: TEST_REGION,
            environment_name: "min-env",
            cname_prefix: Some("abcd".to_string()),
        },
        NameRow {
            case: "a prefix at the length trait's maximum",
            region: TEST_REGION,
            environment_name: "max-env",
            cname_prefix: Some("z".repeat(63)),
        },
        NameRow {
            case: "a prefix in mixed case",
            region: TEST_REGION,
            environment_name: "mixed-case-env",
            cname_prefix: Some("Published-NAME-Mixed".to_string()),
        },
    ]
}

/// Call `action`, failing the named case unless it answered 200.
async fn ok(emulator: &Emulator, case: &str, action: &str, params: &[(&str, &str)]) -> Answer {
    let answer = emulator.call(action, params).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from {action}, got {} and body {}",
        answer.status, answer.body
    );
    answer
}

/// The same, signed for `region`.
async fn ok_in(
    emulator: &Emulator,
    case: &str,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> Answer {
    let answer = emulator.call_in_region(region, action, params).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from {action}, got {} and body {}",
        answer.status, answer.body
    );
    answer
}

/// RULE: the address an environment publishes is a DNS name whose first label is
/// its CNAME prefix and whose second is its Region, followed by a domain — with
/// no path and no character a name cannot carry — so the prefix is what the first
/// dot-label of the value is, and `CheckDNSAvailability` reports that recovered
/// prefix as taken.
#[tokio::test]
async fn an_environment_publishes_the_dns_name_its_prefix_forms() {
    let emulator = Emulator::new();
    ok(
        &emulator,
        "setup",
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;

    for row in rows() {
        let case = row.case;
        let mut params: Vec<(&str, &str)> = vec![
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", row.environment_name),
            ("SolutionStackName", SOLUTION_STACK.as_str()),
        ];
        if let Some(prefix) = row.cname_prefix.as_deref() {
            params.push(("CNAMEPrefix", prefix));
        }
        let launched = ok_in(&emulator, case, row.region, "CreateEnvironment", &params).await;
        let environment_id = launched.expect("EnvironmentId", case).to_string();
        let cname = launched.expect("CNAME", case).to_string();

        // The prefix the launch holds: the one it supplied, or the environment's
        // own name with its unique id body appended for a launch that supplied
        // none.
        let prefix = row.cname_prefix.clone().unwrap_or_else(|| {
            format!(
                "{}-{}",
                row.environment_name,
                environment_id.trim_start_matches("e-")
            )
        });

        // The FORM, as the harness spells it: the prefix label, the Region label,
        // this service's label and the zone this emulator announces in, on the
        // port its environment plane answers at.
        assert_eq!(
            cname,
            emulator.published_cname(row.region, &prefix),
            "case {case:?}: unexpected published CNAME, got body {}",
            launched.body
        );

        // And the properties a client reads it for, asserted on the value itself
        // rather than on the form the harness composed.
        let (host, port) = cname
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("case {case:?}: expected a dialable address, got {cname:?}"));
        assert!(
            port.parse::<u16>().is_ok(),
            "case {case:?}: expected the port the plane answers at, got {port:?} in {cname:?}"
        );
        assert!(
            !host.contains('/') && !host.contains(':'),
            "case {case:?}: a DNS name carries no path and no port inside it, got {host:?}"
        );
        let labels: Vec<&str> = host.split('.').collect();
        assert_eq!(
            labels.first().copied(),
            Some(prefix.as_str()),
            "case {case:?}: the first label must be the environment's CNAME prefix, got {host:?}"
        );
        assert_eq!(
            labels.get(1).copied(),
            Some(row.region),
            "case {case:?}: the second label must be the environment's Region, got {host:?}"
        );
        assert!(
            labels.len() >= 4 && labels[2..].iter().all(|label| !label.is_empty()),
            "case {case:?}: the labels must be followed by a domain carrying a dot, got {host:?}"
        );

        // The documented way back to the prefix — the first dot-label — is a
        // prefix `CheckDNSAvailability` accepts (at least the 4 characters
        // `DNSCnamePrefix` admits) and reports as taken, because this
        // environment holds it.
        let recovered = labels[0];
        assert!(
            recovered.chars().count() >= 4 && eb::state::is_legal_cname_prefix(recovered),
            "case {case:?}: the recovered prefix must be a usable CNAME prefix, got \
             {recovered:?}"
        );
        let checked = ok_in(
            &emulator,
            case,
            row.region,
            "CheckDNSAvailability",
            &[("CNAMEPrefix", recovered)],
        )
        .await;
        assert_eq!(
            checked.value("Available"),
            Some("false"),
            "case {case:?}: the prefix this environment holds must not be available, got body {}",
            checked.body
        );

        // The EndpointURL is the ADDRESS the environment is served at — "the
        // URL to the LoadBalancer" or "the IP address of the instance", not the
        // name — and a later read reports the identical pair: both are the
        // environment's, not the reading caller's.
        let served_at = emulator.environment_address(row.region, &prefix);
        assert_eq!(
            launched.expect("EndpointURL", case),
            served_at,
            "case {case:?}: the EndpointURL must be the address it is served at, got body {}",
            launched.body
        );
        let described = ok_in(
            &emulator,
            case,
            row.region,
            "DescribeEnvironments",
            &[("EnvironmentIds.member.1", environment_id.as_str())],
        )
        .await;
        assert_eq!(
            described.values_at("Environments/member/CNAME"),
            vec![cname.as_str()],
            "case {case:?}: every read reports the same published name, got body {}",
            described.body
        );
        assert_eq!(
            described.values_at("Environments/member/EndpointURL"),
            vec![served_at.as_str()],
            "case {case:?}: every read reports the same EndpointURL, got body {}",
            described.body
        );
    }
}

/// The two bundles the swap below serves, each a static site carrying its own
/// content: a bundle with no runnable entry point serves the file a request
/// addresses, which is what makes the two environments tell themselves apart.
const BLUE_DOCUMENT: &[u8] = b"<!doctype html><title>blue</title><p>blue answers\n";
/// See [`BLUE_DOCUMENT`].
const GREEN_DOCUMENT: &[u8] = b"<!doctype html><title>green</title><p>green answers\n";

/// Upload a bundle carrying `document` as its root document, create a version of
/// [`APPLICATION`] from it, and launch an environment deploying that version under
/// `prefix`.
async fn launched(
    emulator: &Emulator,
    case: &str,
    environment_name: &str,
    prefix: &str,
    version: &str,
    document: &[u8],
) {
    let bucket = format!("{BUNDLE_BUCKET_PREFIX}-{version}");
    let key = "bundles/app.zip";
    emulator
        .put_object(&bucket, key, &zip_of(&[("index.html", document)]))
        .await;
    ok(
        emulator,
        case,
        "CreateApplicationVersion",
        &[
            ("ApplicationName", APPLICATION),
            ("VersionLabel", version),
            ("SourceBundle.S3Bucket", &bucket),
            ("SourceBundle.S3Key", key),
        ],
    )
    .await;
    ok(
        emulator,
        case,
        "CreateEnvironment",
        &[
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", environment_name),
            ("CNAMEPrefix", prefix),
            ("SolutionStackName", SOLUTION_STACK.as_str()),
            ("VersionLabel", version),
        ],
    )
    .await;
}

/// The body the published address `cname` serves for `/`, dialled exactly as the
/// API reported it — the address a client was given, with no credential of any
/// kind.
async fn served(emulator: &Emulator, case: &str, cname: &str) -> String {
    let answer = emulator.get_url(&format!("http://{cname}/")).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected http://{cname}/ to be served by the deployed application, got \
         {} and body {}",
        answer.status, answer.body
    );
    answer.body
}

/// The same question asked over a REAL connection to the socket the plane is
/// served on: the whole HTTP/1.1 answer a client gets for `GET /` of the
/// published address `cname`, with that address in the request's `Host` and
/// nothing else naming the environment.
///
/// This is the client the published address exists for, and it exercises the
/// relay the socket performs — a request whose addressing the front door must
/// read back exactly as the client stated it. The connection is opened to the
/// LOOPBACK literal, the interface the core binds its listeners on and the one a
/// resolver answers a name under the loopback zone with (RFC 6761), so what is
/// under test is the addressing and not the host's own resolver.
async fn dialled(case: &str, cname: &str) -> String {
    let port: u16 = cname
        .rsplit_once(':')
        .and_then(|(_host, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a port in {cname:?}"));
    dialled_request(case, port, cname, "/").await
}

/// One request written on a fresh connection to the plane socket at `port`,
/// stating `host` and asking for `path`, and the whole answer it was given.
async fn dialled_request(case: &str, port: u16, host: &str, path: &str) -> String {
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    let mut connection = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: expected {host} to be dialable, got {error}")
        });
    tokio::io::AsyncWriteExt::write_all(&mut connection, request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be sent, got {error}"));
    let mut answered = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut connection, &mut answered)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the answer must be read, got {error}"));
    String::from_utf8_lossy(&answered).into_owned()
}

/// The published CNAME of the environment `environment_name`, read back through
/// `DescribeEnvironments`.
async fn published(emulator: &Emulator, case: &str, environment_name: &str) -> String {
    read_back(emulator, case, environment_name, "CNAME").await
}

/// The published `EndpointURL` of that environment, read back the same way.
async fn published_endpoint(emulator: &Emulator, case: &str, environment_name: &str) -> String {
    read_back(emulator, case, environment_name, "EndpointURL").await
}

/// One member of the environment `environment_name`, read back through
/// `DescribeEnvironments`.
async fn read_back(
    emulator: &Emulator,
    case: &str,
    environment_name: &str,
    member: &str,
) -> String {
    let described = ok(
        emulator,
        case,
        "DescribeEnvironments",
        &[("EnvironmentNames.member.1", environment_name)],
    )
    .await;
    described
        .expect(&format!("Environments/member/{member}"), case)
        .to_string()
}

/// RULE: the published name is an address this emulator ANSWERS at — a plain HTTP
/// request for it is served by the application of the environment that carries
/// that name — and `SwapEnvironmentCNAMEs` exchanges the two names, so each name
/// is then served by the OTHER environment's application while each environment
/// goes on reporting the name it now holds.
#[tokio::test]
async fn the_published_name_is_served_and_follows_a_swap() {
    let case = "a blue/green swap";
    let emulator = Emulator::with_object_storage();
    ok(
        &emulator,
        case,
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    launched(&emulator, case, "blue-env", "blue-site", "v-blue", BLUE_DOCUMENT).await;
    launched(
        &emulator,
        case,
        "green-env",
        "green-site",
        "v-green",
        GREEN_DOCUMENT,
    )
    .await;
    // "Ready: Environment is available to have an action performed on it, such as
    // update or terminate" — waited for as a CONDITION, never as a fixed time.
    for environment_name in ["blue-env", "green-env"] {
        wait_for_status(&emulator, case, environment_name, eb::state::STATUS_READY).await;
    }

    let blue = published(&emulator, case, "blue-env").await;
    let green = published(&emulator, case, "green-env").await;
    let blue_endpoint = published_endpoint(&emulator, case, "blue-env").await;
    assert_ne!(
        blue, green,
        "case {case:?}: the two environments must publish different names"
    );
    let blue_body = String::from_utf8_lossy(BLUE_DOCUMENT).to_string();
    let green_body = String::from_utf8_lossy(GREEN_DOCUMENT).to_string();
    assert_eq!(
        served(&emulator, case, &blue).await,
        blue_body,
        "case {case:?}: the name blue-env publishes must serve its own application"
    );
    assert_eq!(
        served(&emulator, case, &green).await,
        green_body,
        "case {case:?}: the name green-env publishes must serve its own application"
    );
    // And the address it is SERVED at — "the URL to the LoadBalancer", "the IP
    // address of the instance" — answers for its own environment too.
    assert_eq!(
        served(&emulator, case, &blue_endpoint).await,
        blue_body,
        "case {case:?}: the address blue-env is served at must serve its own application"
    );
    // The same, over a REAL connection to the socket the plane is served on,
    // which is the client the published name exists for.
    let answered = dialled(case, &blue).await;
    assert!(
        answered.starts_with("HTTP/1.1 200") && answered.contains(&blue_body),
        "case {case:?}: a client dialling the name blue-env publishes must be served its own \
         application, got {answered:?}"
    );

    ok(
        &emulator,
        case,
        "SwapEnvironmentCNAMEs",
        &[
            ("SourceEnvironmentName", "blue-env"),
            ("DestinationEnvironmentName", "green-env"),
        ],
    )
    .await;

    // Each environment now reports the name the other held …
    assert_eq!(
        published(&emulator, case, "blue-env").await,
        green,
        "case {case:?}: the swap must give blue-env the name green-env held"
    );
    assert_eq!(
        published(&emulator, case, "green-env").await,
        blue,
        "case {case:?}: the swap must give green-env the name blue-env held"
    );
    // … and each NAME is served by the application of the environment that now
    // carries it, which is the whole consequence of a swap.
    assert_eq!(
        served(&emulator, case, &blue).await,
        green_body,
        "case {case:?}: after the swap the name blue-env held must serve green-env's application"
    );
    assert_eq!(
        served(&emulator, case, &green).await,
        blue_body,
        "case {case:?}: after the swap the name green-env held must serve blue-env's application"
    );
    // Only the NAMES moved: the address each environment is served at is its own,
    // exactly as a load balancer stays with the environment it fronts.
    assert_eq!(
        published_endpoint(&emulator, case, "blue-env").await,
        blue_endpoint,
        "case {case:?}: the swap must leave blue-env's EndpointURL where it was"
    );
    assert_eq!(
        served(&emulator, case, &blue_endpoint).await,
        blue_body,
        "case {case:?}: after the swap blue-env's own address must still serve its own \
         application"
    );
    // Both consequences again over a REAL connection, where the addressing the
    // client stated has to survive the socket's relay to the front door: the NAME
    // blue-env held is now green-env's application, and blue-env's own address is
    // still blue-env's.
    let answered = dialled(case, &blue).await;
    assert!(
        answered.starts_with("HTTP/1.1 200") && answered.contains(&green_body),
        "case {case:?}: a client dialling the name blue-env held must be served green-env's \
         application after the swap, got {answered:?}"
    );
    let answered = dialled(case, &green).await;
    assert!(
        answered.starts_with("HTTP/1.1 200") && answered.contains(&blue_body),
        "case {case:?}: a client dialling the name green-env held must be served blue-env's \
         application after the swap, got {answered:?}"
    );
}

/// RULE: which environment the socket an environment's address is served on
/// relays a request to is decided by the environments this service HOLDS, never
/// by the shape of the host a client wrote — so a host no environment of this
/// store answers at is served nothing, and a request whose PATH names a live
/// environment is served that environment's application whatever host it stated.
#[tokio::test]
async fn an_invented_host_decides_nothing_the_socket_relays() {
    let case = "an invented host";
    let emulator = Emulator::with_object_storage();
    ok(
        &emulator,
        case,
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    launched(&emulator, case, "blue-env", "blue-site", "v-blue", BLUE_DOCUMENT).await;
    wait_for_status(&emulator, case, "blue-env", eb::state::STATUS_READY).await;

    let blue = published(&emulator, case, "blue-env").await;
    let port: u16 = blue
        .rsplit_once(':')
        .and_then(|(_host, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a port in {blue:?}"));
    // A host shaped exactly like one of this service's published names — down to
    // a first label naming another service — that no environment answers at.
    let invented = format!("s3.us-east-1.elasticbeanstalk.localhost:{port}");
    let blue_path = eb::state::environment_plane_path(TEST_REGION, "blue-site");
    let blue_body = String::from_utf8_lossy(BLUE_DOCUMENT).to_string();

    let rows: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "a name no environment of this store answers at",
            invented.as_str(),
            "/",
            None,
        ),
        (
            "that same host, with the plane path of a live environment",
            invented.as_str(),
            blue_path.as_str(),
            Some(blue_body.as_str()),
        ),
        (
            "the loopback address this socket answers on, with that path",
            "127.0.0.1",
            blue_path.as_str(),
            Some(blue_body.as_str()),
        ),
    ];
    for (row, host, path, expected) in rows {
        let answered = dialled_request(row, port, host, path).await;
        match expected {
            Some(body) => assert!(
                answered.starts_with("HTTP/1.1 200") && answered.contains(body),
                "case {row:?}: expected the application named by the path, got {answered:?}"
            ),
            None => assert!(
                answered.starts_with("HTTP/1.1 404"),
                "case {row:?}: expected nothing to be served, got {answered:?}"
            ),
        }
    }
}
