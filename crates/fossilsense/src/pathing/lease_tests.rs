use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use tempfile::tempdir;

use super::{cleanup_index_directory, IndexDbLease};

const LEASE_CHILD_PATH_ENV: &str = "FOSSILSENSE_TEST_GENERATION_LEASE_CHILD_PATH";
const LEASE_CHILD_MARKER_ENV: &str = "FOSSILSENSE_TEST_GENERATION_LEASE_CHILD_MARKER";
const LEASE_CHILD_MARKER_CONTENT: &[u8] = b"generation-lease-acquired\n";

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child already reaped")
    }

    fn terminate_and_wait(&mut self) -> std::result::Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let process_id = child.id();
        let kill_error = child.kill().err();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    return Err(format!(
                        "generation lease child {process_id} did not exit after kill request: \
                         kill_error={kill_error:?}"
                    ));
                }
                Err(error) => {
                    return Err(format!("failed to poll generation lease child: {error}"));
                }
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.terminate_and_wait();
    }
}

#[test]
fn generation_lease_child() {
    let Some(path) = std::env::var_os(LEASE_CHILD_PATH_ENV) else {
        return;
    };
    let marker = std::path::PathBuf::from(
        std::env::var_os(LEASE_CHILD_MARKER_ENV)
            .expect("lease child marker must accompany the database path"),
    );
    let _lease = IndexDbLease::acquire_default_generation(std::path::PathBuf::from(path))
        .expect("acquire generation lease");
    let mut marker_staging_name = marker.as_os_str().to_os_string();
    marker_staging_name.push(format!(".{}.tmp", std::process::id()));
    let marker_staging = std::path::PathBuf::from(marker_staging_name);
    let mut marker_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker_staging)
        .expect("create lease marker staging");
    marker_file
        .write_all(LEASE_CHILD_MARKER_CONTENT)
        .expect("write lease marker");
    marker_file.sync_all().expect("sync lease marker");
    drop(marker_file);
    fs::rename(marker_staging, marker).expect("publish lease marker");

    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("wait for parent stdin close");
}

#[test]
fn cross_process_lease_preserves_old_generation_until_reader_exits() {
    let directory = tempdir().expect("index directory");
    let marker = directory.path().join("lease-ready.marker");
    let old = directory.path().join("index-g1-old.sqlite");
    let active = directory.path().join("index-g2-active.sqlite");
    fs::write(&old, b"old").expect("old generation");
    fs::write(&active, b"active").expect("active generation");
    fs::write(
        directory.path().join("active-index"),
        "index-g2-active.sqlite\n",
    )
    .expect("active manifest");

    let mut child = ChildGuard {
        child: Some(
            Command::new(std::env::current_exe().expect("current test executable"))
                .arg("--exact")
                .arg("pathing::lease_tests::generation_lease_child")
                .arg("--nocapture")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .env(LEASE_CHILD_PATH_ENV, &old)
                .env(LEASE_CHILD_MARKER_ENV, &marker)
                .spawn()
                .expect("spawn generation lease child"),
        ),
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut marker_ready = false;
    let mut early_exit = None;
    while !marker_ready && Instant::now() < deadline {
        marker_ready = fs::read(&marker).is_ok_and(|content| content == LEASE_CHILD_MARKER_CONTENT);
        if let Some(status) = child.child_mut().try_wait().expect("poll lease child") {
            early_exit = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker_ready,
        "lease child did not publish its synced marker; exit={early_exit:?}"
    );

    cleanup_index_directory(directory.path(), SystemTime::now() + Duration::from_secs(1))
        .expect("cleanup while another process reads");
    assert!(
        old.exists(),
        "cleanup must preserve a generation leased by another process"
    );
    cleanup_index_directory(directory.path(), SystemTime::now() + Duration::from_secs(1))
        .expect("repeat cleanup while another process reads");
    assert!(
        old.exists(),
        "repeated cleanup must not split an active lease onto an unlinked inode"
    );

    child
        .terminate_and_wait()
        .expect("terminate generation lease child");
    cleanup_index_directory(directory.path(), SystemTime::now() + Duration::from_secs(1))
        .expect("cleanup after reader exit");
    assert!(
        !old.exists(),
        "old generation should be reclaimed after the reader exits"
    );
}
