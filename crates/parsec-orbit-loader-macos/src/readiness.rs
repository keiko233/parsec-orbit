//! Bounded readiness for the macOS loader.
//!
//! The dependency PoC waited a fixed three seconds on a worker thread before
//! running the core scan. That sleep is a placeholder, not a contract: on a
//! slow machine it can fire before the official client is mapped, and on a
//! fast machine it pointlessly delays activation. The readiness policy waits
//! for evidence that the client has started, with a polling deadline
//! (`docs/spec/macos-host.md`, section 6.3;
//! `docs/design/macos-phases.md`, M1a).
//!
//! Readiness has two observable conditions: a versioned `parsecd-<version>-`
//! `<build>.dylib` image is mapped, and the stub has finished enough of its
//! bootstrap for the dependency to be usable. Both are injected as small
//! closures so unit tests never need a real client: [`parse_client_name`]
//! matches image file names, and [`wait_until_ready`] polls a caller-provided
//! probe with a caller-provided sleeper.

use std::time::{Duration, Instant};

/// Hard cap for [`wait_until_ready`].
///
/// The copy PoC showed the login window within a few seconds of launch, so a
/// healthy client is detected long before this cap. Thirty seconds is a
/// safety fence against hanging the loader worker, not a wait the happy path
/// should ever reach.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between readiness probes.
///
/// A constant ten milliseconds keeps the implementation simple and the added
/// activation latency negligible. There is no exponential backoff: the wait
/// is already bounded by [`READINESS_TIMEOUT`], and backing off would only
/// delay detection of a client that is already up.
pub const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A recognized official client library, split into its two name parts.
///
/// `version` is the client version (for example `"150"` in
/// `parsecd-150-101a.dylib`) and `build` is the build tag (for example
/// `"101a"`). Both are opaque strings; version policy lives with the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientBuild {
    /// The `<version>` part of the client dylib file name.
    pub version: String,
    /// The `<build>` part of the client dylib file name.
    pub build: String,
}

/// The outcome of a single readiness probe.
///
/// Returned by the `probe` closure passed to [`wait_until_ready`]. Keeping
/// the probe behind this enum lets tests script bootstrap sequences without
/// mapping a real client dylib.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeState {
    /// The client is mapped and far enough through bootstrap to continue.
    Ready(ClientBuild),
    /// Not ready yet; the payload explains what is still missing.
    NotYet(String),
    /// A permanent provenance/safety failure, without polling it again.
    Failed(String),
}

/// Matches a client dylib file name of the form `parsecd-<version>-<build>`
/// `.dylib`, where `version` and `build` are non-empty ASCII alphanumerics.
///
/// Only the trailing file-name component is matched, so full image paths are
/// accepted. Anything else yields `None`: a different prefix, a missing
/// `.dylib` suffix, a missing part, or a part holding other characters (for
/// example the `-` in a doubled dash or the `.` in a dotted version).
pub fn parse_client_name(name: &str) -> Option<ClientBuild> {
    let file = name.rsplit('/').next()?;
    let without_prefix = file.strip_prefix("parsecd-")?;
    let without_suffix = without_prefix.strip_suffix(".dylib")?;
    let (version, build) = without_suffix.split_once('-')?;
    if version.is_empty() || build.is_empty() {
        return None;
    }
    let alphanumeric = |part: &str| part.bytes().all(|b| b.is_ascii_alphanumeric());
    if !alphanumeric(version) || !alphanumeric(build) {
        return None;
    }
    Some(ClientBuild {
        version: version.to_string(),
        build: build.to_string(),
    })
}

/// Polls `probe` every [`POLL_INTERVAL`] until it reports ready or `deadline`
/// passes.
///
/// The probe must be nonblocking and return promptly; this synchronous helper
/// cannot interrupt a blocked callback. Callers must obtain filesystem/hash
/// observations outside the probe and supply snapshots. The probe runs at
/// least once, so an already-expired deadline still
/// observes the current state exactly once and never sleeps. Between attempts
/// `sleeper` is called with the current interval; production passes
/// `std::thread::sleep`, while tests pass a virtual sleeper that records the
/// call and returns immediately, so no test actually sleeps. The loop is
/// bounded by the wall clock: once `Instant::now()` reaches `deadline` the
/// `NotYet` reason from that final probe is returned with a timeout
/// explanation instead of blocking the caller forever.
pub fn wait_until_ready<P, S>(
    mut probe: P,
    deadline: Instant,
    mut sleeper: S,
) -> Result<ClientBuild, String>
where
    P: FnMut() -> ProbeState,
    S: FnMut(Duration),
{
    loop {
        match probe() {
            ProbeState::Ready(build) => return Ok(build),
            ProbeState::Failed(reason) => return Err(reason),
            ProbeState::NotYet(reason) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for the client (last state: {})",
                        reason
                    ));
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            sleeper(POLL_INTERVAL.min(remaining));
        }
    }
}

/// Requires both a mapped image under the validated isolated state and a
/// release announcement appended by this launch. `new_log` must exclude
/// all bytes present before the constructor; old log lines are not readiness.
/// An image alone does not prove that wx_main has begun running.
pub fn client_started(
    isolated_state: &std::path::Path,
    mapped_images: &[std::path::PathBuf],
    new_log: &str,
) -> ProbeState {
    for path in mapped_images {
        if path.parent() != Some(isolated_state) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(build) = parse_client_name(name) else {
            continue;
        };
        let announcement = format!("Parsec release ({}-{},", build.version, build.build);
        if new_log
            .lines()
            .any(|line| line.contains(&announcement) && line.contains("Loader: 14)"))
        {
            return ProbeState::Ready(build);
        }
    }
    ProbeState::NotYet(
        "waiting for an isolated mapped client and a new LOADER_V14 release announcement".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn build(version: &str, build: &str) -> ClientBuild {
        ClientBuild {
            version: version.to_string(),
            build: build.to_string(),
        }
    }

    #[test]
    fn parses_the_two_observed_client_names() {
        assert_eq!(
            parse_client_name("parsecd-150-101a.dylib"),
            Some(build("150", "101a"))
        );
        assert_eq!(
            parse_client_name("parsecd-150-104a.dylib"),
            Some(build("150", "104a"))
        );
    }

    #[test]
    fn rejects_the_spec_negative_names() {
        assert_eq!(parse_client_name("parsecd.dylib"), None);
        assert_eq!(parse_client_name("foo-150-101a.dylib"), None);
        assert_eq!(parse_client_name("parsecd-150-.dylib"), None);
        assert_eq!(parse_client_name("parsecd--101a.dylib"), None);
    }

    #[test]
    fn rejects_names_with_non_alphanumeric_parts() {
        assert_eq!(parse_client_name("parsecd-150-10.1.dylib"), None);
        assert_eq!(parse_client_name("parsecd-15_0-101a.dylib"), None);
        assert_eq!(parse_client_name("parsecd-150-101a-foo.dylib"), None);
        assert_eq!(parse_client_name("parsecd-150-101a.so"), None);
        assert_eq!(parse_client_name(""), None);
    }

    #[test]
    fn accepts_a_full_image_path_by_matching_the_trailing_name() {
        assert_eq!(
            parse_client_name("/Applications/Parsec.app/foo/parsecd-150-101a.dylib"),
            Some(build("150", "101a"))
        );
    }

    #[test]
    fn ready_on_the_first_probe_never_sleeps() {
        let slept = RefCell::new(Vec::new());
        let result = wait_until_ready(
            || ProbeState::Ready(build("150", "104a")),
            Instant::now() + Duration::from_secs(5),
            |interval| {
                slept.borrow_mut().push(interval);
            },
        );
        assert_eq!(result, Ok(build("150", "104a")));
        assert!(slept.borrow().is_empty());
    }

    #[test]
    fn polls_at_a_constant_interval_until_ready() {
        let calls = RefCell::new(0usize);
        let slept = RefCell::new(Vec::new());
        let result = wait_until_ready(
            || {
                let mut calls = calls.borrow_mut();
                *calls += 1;
                if *calls < 3 {
                    ProbeState::NotYet(String::from("still starting"))
                } else {
                    ProbeState::Ready(build("150", "104a"))
                }
            },
            Instant::now() + Duration::from_secs(5),
            |interval| {
                slept.borrow_mut().push(interval);
            },
        );
        assert_eq!(result, Ok(build("150", "104a")));
        let slept = slept.borrow();
        assert_eq!(slept.len(), 2);
        assert!(slept.iter().all(|interval| *interval == POLL_INTERVAL));
    }

    #[test]
    fn timeout_returns_the_last_reason_without_sleeping() {
        let slept = RefCell::new(Vec::new());
        let result = wait_until_ready(
            || ProbeState::NotYet(String::from("stub bootstrap incomplete")),
            Instant::now(),
            |interval| {
                slept.borrow_mut().push(interval);
            },
        );
        let error = result.expect_err("an expired deadline must time out");
        assert!(error.contains("stub bootstrap incomplete"), "{error}");
        assert!(error.contains("timed out"), "{error}");
        assert!(slept.borrow().is_empty());
    }

    #[test]
    fn a_mapped_image_without_a_new_client_announcement_is_not_ready() {
        let state = std::path::Path::new("/private/orbit/home/.parsec");
        let images = vec![state.join("parsecd-150-104a.dylib")];
        assert!(matches!(
            client_started(state, &images, ""),
            ProbeState::NotYet(_)
        ));
        assert!(matches!(
            client_started(
                state,
                &images,
                "Parsec release (150-101a, Service: -1, Loader: 14)"
            ),
            ProbeState::NotYet(_)
        ));
        assert!(matches!(
            client_started(
                state,
                &images,
                "Parsec release (150-104a, Service: -1, Loader: 13)"
            ),
            ProbeState::NotYet(_)
        ));
        assert!(matches!(
            client_started(
                state,
                &[],
                "Parsec release (150-104a, Service: -1, Loader: 14)"
            ),
            ProbeState::NotYet(_)
        ));
        assert_eq!(
            client_started(
                state,
                &images,
                "[D] Parsec release (150-104a, Service: -1, Loader: 14)"
            ),
            ProbeState::Ready(build("150", "104a"))
        );
    }

    #[test]
    fn foreign_image_paths_and_permanent_failure_never_activate() {
        let state = std::path::Path::new("/private/orbit/home/.parsec");
        let images = vec![std::path::PathBuf::from(
            "/Users/Shared/.parsec/parsecd-150-104a.dylib",
        )];
        assert!(matches!(
            client_started(
                state,
                &images,
                "Parsec release (150-104a, Service: -1, Loader: 14)"
            ),
            ProbeState::NotYet(_)
        ));
        let result = wait_until_ready(
            || ProbeState::Failed("digest mismatch".into()),
            Instant::now() + READINESS_TIMEOUT,
            |_| panic!("must not sleep"),
        );
        assert_eq!(result, Err("digest mismatch".into()));
    }
}
