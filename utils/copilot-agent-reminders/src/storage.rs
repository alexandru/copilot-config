//! Filesystem persistence for one session's agent reminder state.
//!
//! This module is the filesystem boundary: it stores and retrieves opaque
//! bytes and knows nothing about the state format. Callers keep a
//! [`LockedState`] alive across their read/evolve/write sequence, which holds
//! an exclusive OS lock on the session's stable lock file for that entire
//! sequence.
//!
//! Invariants:
//!
//! - The lock file is created on demand and never removed. Deleting it would
//!   let concurrent processes lock different files and silently lose updates.
//! - The lock belongs to the open file handle inside [`LockedState`]. Dropping
//!   the value closes the handle and releases the lock; no explicit unlock is
//!   needed.
//! - State replacement writes a temporary file in the same session directory
//!   and renames it over the state file. Anyone opening the state path observes
//!   either the complete old contents or the complete new contents.
//! - Replacement deliberately does not fsync. It trades power-loss durability
//!   for low latency, but a failed replacement leaves the previous state file
//!   intact.
//! - Lock acquisition is bounded: contention returns
//!   [`io::ErrorKind::WouldBlock`] after about 100 ms instead of waiting
//!   indefinitely.
//!
//! On Unix, the session directory is restricted to mode 0700 and the lock file
//! to 0600. Temporary and persisted state files are private because
//! `tempfile` creates them with mode 0600.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

const STATE_FILE_NAME: &str = "agent-reminders-state.json";
const LOCK_FILE_NAME: &str = "agent-reminders-state.lock";
const MAX_SESSION_ID_LEN: usize = 128;
const LOCK_WAIT_LIMIT: Duration = Duration::from_millis(100);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(2);

/// Exclusive access to one session's persisted reminder state.
///
/// The value owns the locked file handle. Keep it alive while reading,
/// evolving, and writing state; dropping it releases the lock. [`Self::open`]
/// creates the session directory and its stable lock file when they do not
/// exist; [`Self::open_existing`] requires the directory to already exist.
#[derive(Debug)]
#[must_use = "dropping this value immediately releases the session lock"]
pub(crate) struct LockedState {
    session_directory: PathBuf,
    state_path: PathBuf,
    _lock: File,
}

impl LockedState {
    /// Opens `session_id`'s state directory and acquires its exclusive lock.
    ///
    /// `base_directory` is the shared parent directory chosen by the caller,
    /// for example `~/.agents/sessions/copilot`. The per-session directory is
    /// created inside it when needed.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] when `session_id` is empty,
    /// longer than 128 bytes, or contains anything other than ASCII letters,
    /// digits, hyphens, and underscores. Also returns errors from creating the
    /// session directory or lock file, and [`io::ErrorKind::WouldBlock`] when
    /// the lock is still held after roughly 100 ms.
    pub(crate) fn open(base_directory: &Path, session_id: &str) -> io::Result<Self> {
        validate_session_id(session_id)?;

        let session_directory = base_directory.join(session_id);
        fs::create_dir_all(&session_directory)?;
        #[cfg(unix)]
        restrict_permissions(&session_directory, 0o700)?;

        lock_session_directory(session_directory)
    }

    /// Opens an existing `session_id` state directory and acquires its lock.
    ///
    /// Unlike [`Self::open`], this never creates the per-session directory.
    /// It returns `None` when that directory does not exist, so callers can
    /// inspect or clear state without leaving artifacts for sessions that never
    /// used reminders. A lock file is still created on demand inside an
    /// existing directory when missing.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] when `session_id` is invalid,
    /// [`io::ErrorKind::NotADirectory`] when the session path exists but is not
    /// a directory, and other filesystem errors from opening or locking the
    /// lock file. Also returns [`io::ErrorKind::WouldBlock`] when the lock is
    /// still held after roughly 100 ms. A session directory that disappears
    /// during the call is reported as `None`.
    pub(crate) fn open_existing(
        base_directory: &Path,
        session_id: &str,
    ) -> io::Result<Option<Self>> {
        validate_session_id(session_id)?;

        let session_directory = base_directory.join(session_id);
        match fs::metadata(&session_directory) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!(
                        "session path {} is not a directory",
                        session_directory.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }

        match lock_session_directory(session_directory) {
            Ok(locked) => Ok(Some(locked)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Reads the current state bytes, or `None` when no state file exists.
    ///
    /// # Errors
    ///
    /// Missing state is not an error. Any other filesystem error is returned
    /// unchanged.
    pub(crate) fn read(&self) -> io::Result<Option<Vec<u8>>> {
        match fs::read(&self.state_path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Removes the persisted state file, leaving the stable lock file in place.
    ///
    /// Missing state is not an error, so callers can clear unconditionally.
    ///
    /// # Errors
    ///
    /// Returns errors from removing the state file other than a missing file.
    pub(crate) fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.state_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Atomically replaces the state file with `bytes`.
    ///
    /// The bytes are written and flushed to a temporary file in the session
    /// directory, then renamed over the state file. The previous state remains
    /// in place if the replacement fails. This does not fsync, so the new
    /// contents are not guaranteed to survive a power loss immediately after
    /// this call returns.
    ///
    /// # Errors
    ///
    /// Returns errors from creating, writing, flushing, or persisting the
    /// temporary file.
    pub(crate) fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut temporary = NamedTempFile::new_in(&self.session_directory)?;
        temporary.write_all(bytes)?;
        temporary.flush()?;
        temporary
            .persist(&self.state_path)
            .map(|_| ())
            .map_err(|error| error.error)
    }
}

fn lock_session_directory(session_directory: PathBuf) -> io::Result<LockedState> {
    let state_path = session_directory.join(STATE_FILE_NAME);
    let lock_path = session_directory.join(LOCK_FILE_NAME);
    let lock_file = open_lock_file(&lock_path)?;
    acquire_lock(&lock_file, &lock_path)?;

    Ok(LockedState {
        session_directory,
        state_path,
        _lock: lock_file,
    })
}

fn validate_session_id(session_id: &str) -> io::Result<()> {
    let is_valid = !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID_LEN
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');

    if is_valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session id must contain 1-128 ASCII letters, digits, hyphens, or underscores",
        ))
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }

    let file = options.open(path)?;
    #[cfg(unix)]
    restrict_permissions(path, 0o600)?;
    Ok(file)
}

fn acquire_lock(lock_file: &File, lock_path: &Path) -> io::Result<()> {
    let started = Instant::now();

    loop {
        match lock_file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {
                let elapsed = started.elapsed();
                if elapsed >= LOCK_WAIT_LIMIT {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                            "timed out after {} ms waiting for the state lock at {}",
                            LOCK_WAIT_LIMIT.as_millis(),
                            lock_path.display()
                        ),
                    ));
                }
                thread::sleep(LOCK_RETRY_DELAY.min(LOCK_WAIT_LIMIT - elapsed));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn invalid_session_ids_are_rejected_before_touching_the_filesystem() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let invalid_ids = ["", "../escape", "a/b", "a\\b", "a.b", "with space", "é"];

        for session_id in invalid_ids {
            let error = LockedState::open(base.path(), session_id)
                .expect_err("invalid session id must be rejected");
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "session id {session_id:?}"
            );

            let error = LockedState::open_existing(base.path(), session_id)
                .expect_err("invalid session id must be rejected by open_existing");
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidInput,
                "session id {session_id:?}"
            );
        }

        let too_long = "a".repeat(MAX_SESSION_ID_LEN + 1);
        let error = LockedState::open(base.path(), &too_long)
            .expect_err("overlong session id must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let error = LockedState::open_existing(base.path(), &too_long)
            .expect_err("overlong session id must be rejected by open_existing");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        assert_eq!(
            fs::read_dir(base.path())
                .expect("read base directory")
                .count(),
            0,
            "invalid session ids must not create directories"
        );
    }

    #[test]
    fn maximum_length_session_id_is_accepted() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let session_id = "a".repeat(MAX_SESSION_ID_LEN);

        let state =
            LockedState::open(base.path(), &session_id).expect("maximum length id is valid");
        assert!(state.session_directory.is_dir());
    }

    #[test]
    fn missing_state_reads_as_none_and_written_bytes_round_trip() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let state = LockedState::open(base.path(), "session_1").expect("open session state");
        assert_eq!(state.read().expect("read missing state"), None);

        let payload: &[u8] = b"{\"reminders\":[\"review\"]}";
        state.write(payload).expect("write state");

        assert_eq!(state.read().expect("read state").as_deref(), Some(payload));
        assert!(state.state_path.is_file());
    }

    #[test]
    fn open_existing_missing_session_returns_none_without_artifacts() {
        let base = tempfile::tempdir().expect("create temporary base directory");

        let state = LockedState::open_existing(base.path(), "unknown")
            .expect("missing session is not an error");
        assert!(state.is_none());
        assert_eq!(
            fs::read_dir(base.path())
                .expect("read base directory")
                .count(),
            0,
            "open_existing must not create session artifacts"
        );
    }

    #[test]
    fn open_existing_locks_and_reads_an_existing_session() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let payload: &[u8] = b"existing state";

        {
            let created = LockedState::open(base.path(), "known").expect("open new session");
            created.write(payload).expect("write state");
        }

        let existing = LockedState::open_existing(base.path(), "known")
            .expect("open existing session")
            .expect("existing session must be found");
        assert_eq!(
            existing.read().expect("read state").as_deref(),
            Some(payload)
        );
    }

    #[test]
    fn clear_removes_state_but_keeps_the_lock_and_allows_later_writes() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let state = LockedState::open(base.path(), "cleared").expect("open session state");
        state.write(b"old state").expect("write old state");

        state.clear().expect("clear state");
        assert_eq!(state.read().expect("read cleared state"), None);
        assert!(state.session_directory.join(LOCK_FILE_NAME).is_file());
        assert!(!state.state_path.exists());

        // Clearing a session that has no state is not an error.
        state.clear().expect("clear missing state");

        state.write(b"new state").expect("write after clear");
        assert_eq!(
            state.read().expect("read state after clear").as_deref(),
            Some(&b"new state"[..])
        );
    }

    #[test]
    fn sessions_have_independent_state_and_locks() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let first = LockedState::open(base.path(), "first").expect("open first session");
        // A different session must not block on the first session's lock.
        let second = LockedState::open(base.path(), "second").expect("open second session");

        first.write(b"first state").expect("write first state");
        second.write(b"second state").expect("write second state");

        assert_eq!(
            first.read().expect("read first state").as_deref(),
            Some(&b"first state"[..])
        );
        assert_eq!(
            second.read().expect("read second state").as_deref(),
            Some(&b"second state"[..])
        );
        assert_ne!(first.state_path, second.state_path);
    }

    #[test]
    fn replacement_swaps_all_bytes_and_leaves_no_temporary_files() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let state = LockedState::open(base.path(), "replace").expect("open session state");

        state
            .write(b"the previous state is longer")
            .expect("write previous state");
        state.write(b"new state").expect("replace state");

        assert_eq!(
            state.read().expect("read state").as_deref(),
            Some(&b"new state"[..])
        );

        let names: BTreeSet<OsString> = fs::read_dir(&state.session_directory)
            .expect("read session directory")
            .map(|entry| entry.expect("read directory entry").file_name())
            .collect();
        let expected = BTreeSet::from([
            OsString::from(LOCK_FILE_NAME),
            OsString::from(STATE_FILE_NAME),
        ]);
        assert_eq!(names, expected);
    }

    #[test]
    fn read_propagates_errors_other_than_missing_state() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let state = LockedState::open(base.path(), "broken").expect("open session state");
        fs::create_dir(&state.state_path).expect("replace state path with directory");

        let error = state
            .read()
            .expect_err("reading a directory as state must fail");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_preserves_previous_state() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().expect("create temporary base directory");
        let state = LockedState::open(base.path(), "preserve").expect("open session state");
        state.write(b"previous").expect("write previous state");

        let directory = state.session_directory.clone();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
            .expect("make session directory read-only");
        let result = state.write(b"replacement");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("restore session directory permissions");

        match result {
            Ok(()) => {
                // Privileged users can write despite the read-only directory.
                // The failure-preservation invariant cannot be exercised here.
            }
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(
                    state.read().expect("read state").as_deref(),
                    Some(&b"previous"[..])
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn session_directory_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().expect("create temporary base directory");
        let state = LockedState::open(base.path(), "private").expect("open session state");

        let directory_mode = fs::metadata(&state.session_directory)
            .expect("read session directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);

        let lock_path = state.session_directory.join(LOCK_FILE_NAME);
        let lock_mode = fs::metadata(&lock_path)
            .expect("read lock file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(lock_mode, 0o600);

        state.write(b"secret").expect("write state");
        let state_mode = fs::metadata(&state.state_path)
            .expect("read state file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(state_mode, 0o600);
    }

    #[test]
    fn lock_contention_is_bounded_and_released_on_drop() {
        let base = tempfile::tempdir().expect("create temporary base directory");
        let holder = LockedState::open(base.path(), "busy").expect("open holding state");

        let started = Instant::now();
        let error = LockedState::open(base.path(), "busy")
            .expect_err("second open must not acquire a held lock");
        let elapsed = started.elapsed();

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(
            elapsed >= Duration::from_millis(90),
            "contended open returned too early after {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "contended open was not bounded after {elapsed:?}"
        );

        drop(holder);
        let reacquired = LockedState::open(base.path(), "busy")
            .expect("dropping the holder must release the lock");
        reacquired.write(b"free").expect("write after reacquire");
    }

    #[test]
    fn concurrent_increments_on_one_session_are_not_lost() {
        const THREADS: usize = 4;
        const INCREMENTS_PER_THREAD: u64 = 10;

        let base = tempfile::tempdir().expect("create temporary base directory");
        let session_id = "counter";
        LockedState::open(base.path(), session_id)
            .expect("open counter state")
            .write(b"0")
            .expect("initialize counter");

        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);

        for _ in 0..THREADS {
            let base = base.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(
                move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    barrier.wait();

                    for _ in 0..INCREMENTS_PER_THREAD {
                        let state = LockedState::open(&base, session_id)?;
                        let current = state
                            .read()?
                            .ok_or_else(|| io::Error::other("counter state is missing"))?;
                        let value: u64 = std::str::from_utf8(&current)?.parse()?;
                        state.write((value + 1).to_string().as_bytes())?;
                    }

                    Ok(())
                },
            ));
        }

        for handle in handles {
            handle
                .join()
                .expect("worker thread panicked")
                .expect("worker failed to increment state");
        }

        let state = LockedState::open(base.path(), session_id).expect("open final counter state");
        let bytes = state
            .read()
            .expect("read counter state")
            .expect("counter exists");
        let value: u64 = std::str::from_utf8(&bytes)
            .expect("counter is UTF-8")
            .parse()
            .expect("counter is a number");
        assert_eq!(value, THREADS as u64 * INCREMENTS_PER_THREAD);
    }
}
