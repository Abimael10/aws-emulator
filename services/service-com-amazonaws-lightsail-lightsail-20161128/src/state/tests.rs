// SPDX-License-Identifier: CC0-1.0

//! The unit tests of this module's own rules.

use super::*;

/// A plausible instant to create a set of credentials at, and the credentials
/// one is created with. The username is shaped like one the credential store
/// mints; the password stands in for the secret it mints.
const CREATED_AT_MS: i64 = 1_700_000_000_000;
const USERNAME: &str = "EMULATORASJ4K7QWRT2X";
const PASSWORD: &str = "a-minted-registry-password";
const REGION: &str = "us-east-1";

/// One store holding one set of registry log in credentials.
fn store_with_one_login() -> ComAmazonawsLightsailLightsail20161128State {
    let mut store = ComAmazonawsLightsailLightsail20161128State::default();
    store.insert_registry_login(
        REGION,
        StoredRegistryLogin::new(USERNAME.to_string(), PASSWORD, CREATED_AT_MS),
    );
    store
}

/// RULE: the CLOCK is what ends a set of container image registry log in
/// credentials — it is recognised for the documented 12 hours after it was
/// created and gone from the store from that instant on, whether or not
/// anybody called in between. The integration suite asserts the `expiresAt`
/// the caller is told; this asserts the instant itself, which no wall-clock
/// test can reach.
#[test]
fn a_registry_login_is_forgotten_once_the_clock_passes_its_expiry() {
    let expiry = CREATED_AT_MS + REGISTRY_LOGIN_LIFETIME_MS;
    // (case, the instant the clock has reached, whether the set logs in)
    let cases = [
        ("the instant it was created", CREATED_AT_MS, true),
        ("one millisecond before it expires", expiry - 1, true),
        ("the instant it expires", expiry, false),
        ("an hour after it expired", expiry + 60 * 60 * 1_000, false),
    ];
    for (case, at_ms, want) in cases {
        // A fresh store per case: the clock only ever moves forward, so each
        // instant is reached from the creation rather than from the last case.
        let mut store = store_with_one_login();
        store.advance_clock(at_ms);
        let found = store.registry_login(REGION, USERNAME, PASSWORD).is_some();
        assert_eq!(
            found, want,
            "case {case}: expected the set to log in = {want}, got {found}"
        );
        assert_eq!(
            store.registry_logins.contains_key(REGION),
            want,
            "case {case}: expected an expired set to be gone from the store, got \
                 {:?}",
            store.registry_logins.get(REGION).map(Vec::len)
        );
    }
}

/// RULE: the store's own per-region bound keeps the credentials it holds
/// finite: past [`MAX_REGISTRY_LOGINS_PER_REGION`] the OLDEST set is the one
/// dropped, and every younger set still logs in.
#[test]
fn the_oldest_registry_login_is_the_one_the_bound_drops() {
    let mut store = ComAmazonawsLightsailLightsail20161128State::default();
    // One set past the bound, each a millisecond younger than the last so
    // "oldest" is well defined.
    let total = MAX_REGISTRY_LOGINS_PER_REGION + 1;
    let username = |index: usize| format!("{USERNAME}{index}");
    for index in 0..total {
        store.insert_registry_login(
            REGION,
            StoredRegistryLogin::new(
                username(index),
                PASSWORD,
                CREATED_AT_MS + i64::try_from(index).expect("the index fits"),
            ),
        );
    }
    store.advance_clock(CREATED_AT_MS);

    assert_eq!(
        store.registry_logins.get(REGION).map(Vec::len),
        Some(MAX_REGISTRY_LOGINS_PER_REGION),
        "case the region at its bound: expected {MAX_REGISTRY_LOGINS_PER_REGION} sets"
    );
    assert!(
        store
            .registry_login(REGION, &username(0), PASSWORD)
            .is_none(),
        "case the oldest set: expected it to have been dropped"
    );
    for index in 1..total {
        assert!(
            store
                .registry_login(REGION, &username(index), PASSWORD)
                .is_some(),
            "case the set created {index} milliseconds in: expected it to still log in"
        );
    }
}

/// RULE: only the credentials a set was created with log in to it, and only at
/// the registry of the region it was created for.
#[test]
fn only_the_credentials_a_registry_login_was_created_with_are_accepted() {
    let mut store = store_with_one_login();
    store.advance_clock(CREATED_AT_MS);

    // (case, region, username, password)
    let refused = [
        ("another region's registry", "us-west-2", USERNAME, PASSWORD),
        ("a username no set was created for", REGION, "AWS", PASSWORD),
        ("another password", REGION, USERNAME, "another-password"),
        ("the password of the username", REGION, USERNAME, USERNAME),
        ("no password at all", REGION, USERNAME, ""),
        ("a password one character short", REGION, USERNAME, &PASSWORD[..PASSWORD.len() - 1]),
    ];
    for (case, region, username, password) in refused {
        assert!(
            store.registry_login(region, username, password).is_none(),
            "case {case}: expected no set to be recognised"
        );
    }
    assert!(
        store
            .registry_login(REGION, USERNAME, PASSWORD)
            .is_some_and(|login| login.expires_at_ms
                == CREATED_AT_MS + REGISTRY_LOGIN_LIFETIME_MS),
        "case the credentials it was created with: expected the set to be recognised, \
             expiring 12 hours after it was created"
    );
}

/// RULE: what one region's registry holds is accounted for in BYTES — every
/// entry charged what it costs this process, content it already holds and a
/// tag it already has costing it nothing, and a fresh tag of a manifest it
/// already holds costing what that tag takes — so nothing a client repeats
/// grows it without bound.
#[test]
fn a_registry_accounts_for_every_byte_it_holds() {
    let mut registry = StoredImageRegistry::default();
    let layer = b"a layer of image content".to_vec();
    let manifest = br#"{"schemaVersion":2,"layers":[]}"#.to_vec();
    let media_type = "application/vnd.oci.image.manifest.v1+json";
    let layer_digest = registry_digest(&layer);
    // One entry's cost: its own bytes, the string it is keyed by, and the
    // fixed overhead every entry carries.
    let blob_bytes =
        REGISTRY_ENTRY_OVERHEAD_BYTES + layer_digest.len() + layer.len();

    assert_eq!(
        registry.bytes_held(),
        0,
        "case an empty registry: expected it to hold nothing"
    );
    let stored = registry.put_blob(layer.clone(), CREATED_AT_MS);
    assert_eq!(
        (stored, registry.bytes_held()),
        (RegistryWrite::Stored(layer_digest.clone()), blob_bytes),
        "case one pushed layer: expected its own bytes, its digest key and one entry's \
             overhead to be held under its own digest"
    );
    // The SAME bytes again are the blob that is already there.
    registry.put_blob(layer.clone(), CREATED_AT_MS + 1);
    assert_eq!(
        (registry.blobs.len(), registry.bytes_held()),
        (1, blob_bytes),
        "case the same layer pushed twice: expected one blob and no growth"
    );

    let digest = registry_digest(&manifest);
    let manifest_bytes = REGISTRY_ENTRY_OVERHEAD_BYTES
        + digest.len()
        + manifest.len()
        + "app".len()
        + media_type.len();
    let tag_bytes = |tag: &str| {
        REGISTRY_ENTRY_OVERHEAD_BYTES + "app".len() + tag.len() + digest.len()
    };
    for (case, tag, want_tags, want_bytes) in [
        (
            "the manifest under its first tag",
            "latest",
            1,
            blob_bytes + manifest_bytes + tag_bytes("latest"),
        ),
        (
            "the same manifest under the same tag again",
            "latest",
            1,
            blob_bytes + manifest_bytes + tag_bytes("latest"),
        ),
        (
            "the same manifest under a second tag",
            "v1",
            2,
            blob_bytes + manifest_bytes + tag_bytes("latest") + tag_bytes("v1"),
        ),
    ] {
        let stored = registry.put_manifest(
            "app",
            Some(tag),
            manifest.clone(),
            media_type,
            CREATED_AT_MS,
        );
        assert_eq!(
            stored,
            RegistryWrite::Stored(digest.clone()),
            "case {case}: expected it to be held under the digest of its own bytes"
        );
        assert_eq!(
            (registry.manifests.len(), registry.tags.len(), registry.bytes_held()),
            (1, want_tags, want_bytes),
            "case {case}: expected one manifest, {want_tags} tags and {want_bytes} bytes"
        );
    }
}

/// RULE: a registry entry is charged what it costs this process, so the
/// per-region byte bound bounds the ENTRY COUNT too — a client pushing
/// distinct one-byte blobs is refused after at most
/// `MAX_REGISTRY_BYTES_PER_REGION / REGISTRY_ENTRY_OVERHEAD_BYTES` of them,
/// rather than after the hundred million the content alone would admit.
#[test]
fn a_registry_charges_every_entry_its_own_footprint() {
    // Distinct one-byte blobs: the cheapest content a client can push, and
    // each one a digest of its own, so none of them is the blob that is
    // already there.
    let digest_len = registry_digest(&[0]).len();
    for count in [1usize, 2, 8, 64] {
        let mut registry = StoredImageRegistry::default();
        for index in 0..count {
            let content = vec![u8::try_from(index).expect("the table stays under 256")];
            assert_eq!(
                registry.put_blob(content.clone(), CREATED_AT_MS),
                RegistryWrite::Stored(registry_digest(&content)),
                "case {count} one-byte blobs: expected blob {index} to be held"
            );
        }
        let want = count * (REGISTRY_ENTRY_OVERHEAD_BYTES + digest_len + 1);
        assert_eq!(
            (registry.blobs.len(), registry.bytes_held()),
            (count, want),
            "case {count} one-byte blobs: expected each to be charged its digest key and \
                 its own overhead, not just its one byte"
        );
    }

    // The bound that follows: every entry costs at least the per-entry
    // overhead, so one region's registry holds at most this many entries
    // however small the content pushed into them is.
    let ceiling = MAX_REGISTRY_BYTES_PER_REGION / REGISTRY_ENTRY_OVERHEAD_BYTES;
    assert!(
        ceiling <= 1_048_576,
        "expected the per-region bound to admit at most a million entries whatever their \
             content, got {ceiling}"
    );
}

/// RULE: the store's own per-region bound keeps the blob uploads in flight
/// finite: past [`MAX_REGISTRY_UPLOADS_PER_REGION`] the OLDEST session is
/// the one dropped, and every younger session is still there to send to.
#[test]
fn the_oldest_blob_upload_is_the_one_the_bound_drops() {
    let mut registry = StoredImageRegistry::default();
    // One session past the bound, each a millisecond younger than the last
    // so "oldest" is well defined.
    let total = MAX_REGISTRY_UPLOADS_PER_REGION + 1;
    let session = |index: usize| format!("session-{index:04}");
    for index in 0..total {
        registry.start_upload(
            session(index),
            "app",
            CREATED_AT_MS + i64::try_from(index).expect("the index fits"),
        );
    }

    assert_eq!(
        registry.uploads.len(),
        MAX_REGISTRY_UPLOADS_PER_REGION,
        "case the region at its bound: expected {MAX_REGISTRY_UPLOADS_PER_REGION} sessions"
    );
    assert_eq!(
        registry.append_to_upload(&session(0), "app", b"bytes for a dropped session"),
        UploadAppend::Unknown,
        "case the oldest session: expected it to have been dropped"
    );
    for index in 1..total {
        assert_eq!(
            registry.append_to_upload(&session(index), "app", b"ab"),
            UploadAppend::Appended { held: 2 },
            "case the session started {index} milliseconds in: expected it to take bytes"
        );
    }
}

/// RULE: a label's version is one past the highest that label's registered
/// images carry — 1 for the first, 2 for the next, and counted per label —
/// which is what keeps the name of every image a service holds distinct.
#[test]
fn a_registered_image_takes_the_next_version_of_its_label() {
    let mut service = StoredContainerService {
        name: "svc".to_string(),
        arn: String::new(),
        created_at_ms: CREATED_AT_MS,
        location: StoredLocation {
            region_name: REGION.to_string(),
            availability_zone: Some(format!("{REGION}a")),
        },
        tags: Vec::new(),
        power: "nano".to_string(),
        power_id: "nano-1".to_string(),
        scale: 1,
        state: CONTAINER_SERVICE_READY,
        is_disabled: false,
        domain_token: "token".to_string(),
        principal_arn: String::new(),
        public_domain_names: BTreeMap::new(),
        ecr_image_puller_role_active: false,
        deployments: VecDeque::new(),
        next_deployment_version: 1,
        images: Vec::new(),
        owed: VecDeque::new(),
    };

    // (the label registered, the name the service must assign)
    let cases = [
        ("web", ":svc.web.1"),
        ("web", ":svc.web.2"),
        ("api", ":svc.api.1"),
        ("web", ":svc.web.3"),
        ("api", ":svc.api.2"),
    ];
    for (label, want) in cases {
        let registered = service.register_image(label, "sha256:abc", CREATED_AT_MS);
        assert_eq!(
            registered.name(&service.name),
            want,
            "case registering {label:?}: expected the name {want}"
        );
    }
    let held: Vec<String> = service
        .images
        .iter()
        .map(|image| image.name(&service.name))
        .collect();
    assert_eq!(
        held,
        cases.iter().map(|(_, name)| (*name).to_string()).collect::<Vec<_>>(),
        "case the whole registry: expected the images in registration order"
    );
}

/// RULE: a deployment that steps down when the clock activates the next one
/// stays one that PREVIOUSLY RAN, so the containers it declared stay ones
/// the service is running or previously ran, while a deployment that never
/// became active — one still waiting on the clock, or one replaced before it
/// activated — launched nothing and declares none of them. A deployment the
/// [`MAX_DEPLOYMENTS_PER_SERVICE`] bound dropped is forgotten with its
/// containers.
#[test]
fn a_superseded_deployment_is_kept_as_one_that_previously_ran() {
    let mut service = StoredContainerService {
        name: "svc".to_string(),
        arn: String::new(),
        created_at_ms: CREATED_AT_MS,
        location: StoredLocation {
            region_name: REGION.to_string(),
            availability_zone: Some(format!("{REGION}a")),
        },
        tags: Vec::new(),
        power: "nano".to_string(),
        power_id: "nano-1".to_string(),
        scale: 1,
        state: CONTAINER_SERVICE_READY,
        is_disabled: false,
        domain_token: "token".to_string(),
        principal_arn: String::new(),
        public_domain_names: BTreeMap::new(),
        ecr_image_puller_role_active: false,
        deployments: VecDeque::new(),
        next_deployment_version: 1,
        images: Vec::new(),
        owed: VecDeque::new(),
    };

    // Each deployment declares one container named after it, so which
    // deployment a container came from is unambiguous.
    let container_of = |version: i32| format!("app-{version}");
    let deployment = |version: i32| StoredDeployment {
        version,
        state: DEPLOYMENT_STATE_ACTIVATING,
        activated: false,
        containers: BTreeMap::from([(
            container_of(version),
            Container {
                image: Some("nginx:latest".to_string()),
                ..Default::default()
            },
        )]),
        public_endpoint: None,
        created_at_ms: CREATED_AT_MS,
    };

    // Three deployments past the bound, so what the bound drops is covered
    // as well as what it keeps.
    let total = i32::try_from(MAX_DEPLOYMENTS_PER_SERVICE + 3)
        .expect("the case count fits a deployment version");
    for version in 1..=total {
        service.record_deployment(deployment(version));
        service.owed.push_back(OwedPhase {
            at_ms: CREATED_AT_MS,
            state: CONTAINER_SERVICE_RUNNING,
            activates_next_deployment: true,
        });
        service.advance(CREATED_AT_MS);

        let ran = service.deployments_that_ran().count();
        let want = usize::try_from(version)
            .expect("the version fits a count")
            .min(MAX_DEPLOYMENTS_PER_SERVICE);
        assert_eq!(
            ran, want,
            "case {version} deployments activated: expected {want} of them still counted as \
                 having run, got {ran}"
        );
        // Every deployment still held answers for its own container,
        // whether it is the active one or one that ran before.
        for held in service
            .deployments_that_ran()
            .map(|deployment| deployment.version)
            .collect::<Vec<i32>>()
        {
            let container = container_of(held);
            assert!(
                service.has_run_container(&container),
                "case {version} deployments activated: expected {container:?} of the \
                     deployment {held} the store still holds to be one that ran, got the \
                     containers {:?}",
                service.containers_that_ran()
            );
        }
    }

    assert_eq!(
        service
            .current_deployment()
            .map(|active| (active.version, active.state)),
        Some((total, DEPLOYMENT_STATE_ACTIVE)),
        "case the newest deployment: expected it active, got {:?}",
        service.current_deployment()
    );
    let not_inactive: Vec<i32> = service
        .deployments
        .iter()
        .filter(|held| held.version != total && held.state != DEPLOYMENT_STATE_INACTIVE)
        .map(|held| held.version)
        .collect();
    assert!(
        not_inactive.is_empty(),
        "case every superseded deployment: expected each marked \
             {DEPLOYMENT_STATE_INACTIVE}, got {not_inactive:?} that are not"
    );
    // The oldest deployments are the ones the bound forgot, so their
    // containers are no longer ones this service ran.
    let bound = i32::try_from(MAX_DEPLOYMENTS_PER_SERVICE)
        .expect("the bound fits a deployment version");
    for forgotten in 1..=(total - bound) {
        let container = container_of(forgotten);
        assert!(
            !service.has_run_container(&container),
            "case the deployment {forgotten} the bound dropped: expected {container:?} to \
                 be forgotten with it, got the containers {:?}",
            service.containers_that_ran()
        );
    }
    let newest = container_of(total);
    assert!(
        service.containers_that_ran().contains(&newest.as_str()),
        "case the containers that ran: expected {newest:?} among them, got {:?}",
        service.containers_that_ran()
    );

    // A deployment still WAITING on the clock, and one replaced while it was
    // still waiting, both launched nothing — asserted here, where the clock
    // is explicit and no activation can race the check.
    let replaced_while_waiting = total.saturating_add(1);
    let waiting = total.saturating_add(2);
    service.record_deployment(deployment(replaced_while_waiting));
    service.record_deployment(deployment(waiting));
    for (case, version) in [
        ("a deployment still waiting on the clock", waiting),
        (
            "a deployment replaced before it ever activated",
            replaced_while_waiting,
        ),
    ] {
        let container = container_of(version);
        assert!(
            !service.has_run_container(&container),
            "case {case}: expected {container:?} not to be one that ran, got the containers \
                 {:?}",
            service.containers_that_ran()
        );
        assert!(
            service
                .deployments_that_ran()
                .all(|ran| ran.version != version),
            "case {case}: expected it outside the deployments that ran, got {:?}",
            service
                .deployments_that_ran()
                .map(|ran| ran.version)
                .collect::<Vec<i32>>()
        );
    }
}

/// RULE: the access settings this store names are the modelled
/// `ResourceBucketAccess` enum's own members, so the generated value list
/// stays the one source for what the wire accepts and these constants only
/// say which member each one is.
#[test]
fn each_named_bucket_access_setting_is_a_member_of_the_modelled_enum() {
    let generated = crate::ops::types::RESOURCE_BUCKET_ACCESS_VALUES;
    for (case, named) in [
        ("the setting that grants access", RESOURCE_BUCKET_ACCESS_ALLOW),
        ("the setting that revokes it", RESOURCE_BUCKET_ACCESS_DENY),
    ] {
        assert!(
            generated.contains(&named),
            "case {case}: expected {named:?} to be one of the modelled values \
                 {generated:?}"
        );
    }
    assert_ne!(
        RESOURCE_BUCKET_ACCESS_ALLOW, RESOURCE_BUCKET_ACCESS_DENY,
        "case the two settings: expected them to name different members"
    );
    assert_eq!(
        generated.len(),
        2,
        "case the modelled enum: expected the two members these constants cover, got \
             {generated:?}"
    );
}

// ---------------------------------------------------------------------------
// The metric series
// ---------------------------------------------------------------------------

/// The instant a resource of these cases started publishing, in epoch
/// milliseconds. A fixed instant, so every window a case names is the same on
/// every run — which is the point of asserting the cutting rule here: a window
/// wider than a test can live through is unobservable over the wire.
const PUBLISHED_FROM_MS: i64 = 1_700_000_000_000;
/// The granularity the cases below READ at (`period`): five minutes, in seconds
/// and in milliseconds.
const CASE_PERIOD_SECONDS: i64 = 300;
/// See [`CASE_PERIOD_SECONDS`].
const CASE_PERIOD_MS: i64 = CASE_PERIOD_SECONDS * 1_000;

/// The granularity the metric these cases read is PUBLISHED at: one minute, the
/// granularity of an instance's status checks.
const CASE_PUBLISH_INTERVAL_MS: i64 = STATUS_CHECK_GRANULARITY_SECONDS * 1_000;

/// What one sample of the metric these cases read measured: the cutting rule
/// under test is about the WINDOWS, so a single stand-in measurement is enough
/// and every statistic of a window follows from it.
const CASE_SAMPLE: f64 = 7.5;

/// The statistics the cases below ask for: every member, so each datapoint
/// carries its sample count too.
fn every_statistic() -> Vec<String> {
    [
        STATISTIC_MINIMUM,
        STATISTIC_MAXIMUM,
        STATISTIC_SUM,
        STATISTIC_AVERAGE,
        STATISTIC_SAMPLE_COUNT,
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

/// One read of a series over `start_ms..end_ms` at `period_seconds`, as of
/// `now_ms`, of a metric published every [`CASE_PUBLISH_INTERVAL_MS`].
fn series_at(
    period_seconds: i64,
    start_ms: i64,
    end_ms: i64,
    now_ms: i64,
    statistics: &[String],
) -> Vec<crate::ops::types::MetricDatapoint> {
    MetricSeries {
        published_from_ms: PUBLISHED_FROM_MS,
        publish_interval_ms: CASE_PUBLISH_INTERVAL_MS,
        read: MetricRead {
            metric_name: "StatusCheckFailed",
            unit: "Count",
            period_seconds,
            start_ms,
            end_ms,
            now_ms,
            statistics,
        },
    }
    .datapoints(CASE_SAMPLE)
}

/// The epoch milliseconds of each datapoint's window.
fn window_starts(series: &[crate::ops::types::MetricDatapoint]) -> Vec<i64> {
    series
        .iter()
        .map(|datapoint| {
            datapoint
                .timestamp
                .expect("every datapoint carries its window's instant")
                .to_millis()
                .expect("the window's instant is within epoch milliseconds")
        })
        .collect()
}

/// RULE: the granularity divides the time line into windows, and the answer
/// carries one datapoint per window the resource has published in that the
/// request overlaps — oldest first, each timestamped at its own window's start.
///
/// `PUBLISHED_FROM_MS` is 200 seconds into its own five-minute window
/// (1_699_999_800_000), and the resource has been publishing for twenty minutes,
/// so the windows are that one and the four after it.
#[test]
fn a_read_carries_one_datapoint_per_granularity_window_published_in() {
    let statistics = every_statistic();
    let now_ms = PUBLISHED_FROM_MS + 20 * 60 * 1_000;
    let series = series_at(
        CASE_PERIOD_SECONDS,
        // A window reaching back an hour before the resource existed and
        // forward to now: what the resource published is all of what it can
        // carry.
        PUBLISHED_FROM_MS - 60 * 60 * 1_000,
        now_ms,
        now_ms,
        &statistics,
    );

    assert_eq!(
        window_starts(&series),
        vec![
            1_699_999_800_000,
            1_700_000_100_000,
            1_700_000_400_000,
            1_700_000_700_000,
            1_700_001_000_000,
        ],
        "case a twenty-minute publishing span read at five-minute granularity: expected the \
         window the resource started publishing in and the four after it"
    );

    // Every window the publishing span covers WHOLLY holds one sample per
    // PUBLISHED granularity interval — the reported `sampleCount` is the
    // metric's own granularity in the window, not an artefact of anything this
    // store chose. The first and the last window are clipped by the start of
    // publishing and by now, so they hold fewer — but never none, or they would
    // not be in the answer at all.
    let whole_window_samples = CASE_PERIOD_MS / CASE_PUBLISH_INTERVAL_MS;
    for (index, datapoint) in series.iter().enumerate() {
        let samples = datapoint
            .sample_count
            .expect("the request asked for the sample count") as i64;
        let clipped = index == 0 || index == series.len() - 1;
        if clipped {
            assert!(
                (1..=whole_window_samples).contains(&samples),
                "case the clipped window at index {index}: expected between one and \
                 {whole_window_samples} samples, got {samples}"
            );
        } else {
            assert_eq!(
                samples, whole_window_samples,
                "case the whole window at index {index}: expected {whole_window_samples} \
                 samples"
            );
        }
        let (minimum, maximum, average, sum) = (
            datapoint.minimum.expect("the minimum was asked for"),
            datapoint.maximum.expect("the maximum was asked for"),
            datapoint.average.expect("the average was asked for"),
            datapoint.sum.expect("the sum was asked for"),
        );
        assert!(
            (minimum - CASE_SAMPLE).abs() <= 1e-9
                && (maximum - CASE_SAMPLE).abs() <= 1e-9
                && (average - CASE_SAMPLE).abs() <= 1e-9,
            "case the window at index {index}: every sample of it is {CASE_SAMPLE}, so the \
             extremes and the mean are too, got {minimum}, {average}, {maximum}"
        );
        assert!(
            (sum - average * samples as f64).abs() <= 1e-6,
            "case the window at index {index}: expected the sum to be the average over the \
             samples, got {sum}"
        );
    }
}

/// RULE: a window the resource holds no sample in carries no datapoint — one
/// that closed before it started publishing, and one that has not happened yet.
#[test]
fn a_window_outside_the_publishing_span_carries_no_datapoint() {
    let statistics = every_statistic();
    let now_ms = PUBLISHED_FROM_MS + 20 * 60 * 1_000;
    let hour_ms = 60 * 60 * 1_000;
    for (case, start_ms, end_ms) in [
        (
            "a window that closed before the resource existed",
            PUBLISHED_FROM_MS - 2 * hour_ms,
            PUBLISHED_FROM_MS - hour_ms,
        ),
        (
            "a window that closes at the instant publishing began",
            PUBLISHED_FROM_MS - hour_ms,
            PUBLISHED_FROM_MS + 1,
        ),
        (
            "a window that has not happened yet",
            now_ms + hour_ms,
            now_ms + 2 * hour_ms,
        ),
    ] {
        let series = series_at(CASE_PERIOD_SECONDS, start_ms, end_ms, now_ms, &statistics);
        assert!(
            series.is_empty(),
            "case {case}: expected no datapoint, got {} of them",
            series.len()
        );
    }
}

/// RULE: the answer is bounded. A window and a granularity are both
/// caller-sized, so a request whose quotient is larger than
/// [`MAX_METRIC_DATAPOINTS`] is answered with that many datapoints — the most
/// RECENT windows of the requested span, which is what a caller watching a
/// metric is reading for.
#[test]
fn a_series_longer_than_the_answer_cap_keeps_the_most_recent_windows() {
    let statistics = every_statistic();
    // Read at the granularity the metric is published at, so every window of
    // the span holds a sample and the cap is what decides the length.
    let period_seconds = CASE_PUBLISH_INTERVAL_MS / 1_000;
    let period_ms = CASE_PUBLISH_INTERVAL_MS;
    // Ten windows more than the cap, so the oldest ten are the ones left out.
    let windows = MAX_METRIC_DATAPOINTS as i64 + 10;
    let now_ms = PUBLISHED_FROM_MS + windows * period_ms;
    let series = series_at(
        period_seconds,
        PUBLISHED_FROM_MS - period_ms,
        now_ms + period_ms,
        now_ms,
        &statistics,
    );

    assert_eq!(
        series.len(),
        MAX_METRIC_DATAPOINTS,
        "case a span of {windows} windows: expected the answer capped at \
         {MAX_METRIC_DATAPOINTS}"
    );
    let starts = window_starts(&series);
    assert_eq!(
        starts.last().copied(),
        Some(now_ms.div_euclid(period_ms) * period_ms),
        "case the newest window: expected the one `now` falls in, got {:?}",
        starts.last()
    );
    assert!(
        starts.windows(2).all(|pair| pair[1] - pair[0] == period_ms),
        "case the kept windows: expected one per granularity window, oldest first"
    );
}

/// RULE: a metric is published at the granularity its own documentation states
/// — one minute for an instance's three status checks, five minutes for every
/// other instance metric — so that, and not anything this store chose, is what
/// decides how many samples a window holds.
#[test]
fn each_instance_metric_is_published_at_its_documented_granularity() {
    for (case, metric_name, want) in [
        (
            "both status checks together",
            "StatusCheckFailed",
            STATUS_CHECK_GRANULARITY_SECONDS,
        ),
        (
            "the instance status check",
            "StatusCheckFailed_Instance",
            STATUS_CHECK_GRANULARITY_SECONDS,
        ),
        (
            "the system status check",
            "StatusCheckFailed_System",
            STATUS_CHECK_GRANULARITY_SECONDS,
        ),
        (
            "a utilization gauge",
            "CPUUtilization",
            INSTANCE_METRIC_GRANULARITY_SECONDS,
        ),
        (
            "a transfer counter",
            "NetworkIn",
            INSTANCE_METRIC_GRANULARITY_SECONDS,
        ),
        (
            "a system disk counter",
            "DiskReadOps",
            INSTANCE_METRIC_GRANULARITY_SECONDS,
        ),
        (
            "the metadata service counter",
            "MetadataNoToken",
            INSTANCE_METRIC_GRANULARITY_SECONDS,
        ),
    ] {
        let granularity = instance_metric_granularity_seconds(metric_name);
        assert_eq!(
            granularity, want,
            "case {case}: expected {metric_name} to be published every {want} seconds"
        );
    }
}
