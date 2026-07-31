use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::*;
use crate::pathing::default_index_directory;
use crate::store::IndexWriterLock;

const DEFAULT_WRITER_CHILD_ROOT_ENV: &str = "FOSSILSENSE_TEST_DEFAULT_WRITER_CHILD_ROOT";
const WRITER_CRASH_CHILD_ROOT_ENV: &str = "FOSSILSENSE_TEST_WRITER_CRASH_CHILD_ROOT";
const WRITER_CRASH_MARKER_ENV: &str = "FOSSILSENSE_TEST_WRITER_CRASH_MARKER";
const WRITER_CRASH_MARKER_CONTENT: &[u8] = b"writer-lock-acquired\n";
const WRITER_CRASH_HELPER_TIMEOUT_EXIT: i32 = 93;

struct TestIndexDirectory(std::path::PathBuf);

impl Drop for TestIndexDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child already reaped")
    }

    fn terminate(&mut self) -> std::result::Result<(), String> {
        if let Some(mut child) = self.0.take() {
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
                            "lock holder {process_id} did not exit after kill request: {kill_error:?}"
                        ));
                    }
                    Err(error) => {
                        return Err(format!(
                            "failed to reap lock holder {process_id}: {error}; kill error: {kill_error:?}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[test]
fn default_index_writer_lock_child() {
    let Some(root) = std::env::var_os(DEFAULT_WRITER_CHILD_ROOT_ENV) else {
        return;
    };
    let error = index_workspace(
        std::path::PathBuf::from(root),
        IndexOptions {
            force: true,
            ..Default::default()
        },
        |_| {},
    )
    .expect_err("the parent process must own the default index writer lock");
    assert!(
        error.to_string().contains("locked"),
        "unexpected competing writer error: {error:#}"
    );
}

#[test]
fn index_writer_lock_crash_child() {
    let Some(root) = std::env::var_os(WRITER_CRASH_CHILD_ROOT_ENV) else {
        return;
    };
    let marker = std::path::PathBuf::from(
        std::env::var_os(WRITER_CRASH_MARKER_ENV)
            .expect("crash marker must accompany the child root"),
    );
    let index_directory =
        default_index_directory(&std::path::PathBuf::from(root)).expect("index directory");
    let logical_destination = index_directory.join("index.sqlite");
    let _lock = IndexWriterLock::acquire(&logical_destination).expect("child writer lock");
    let mut marker_staging_name = marker.as_os_str().to_os_string();
    marker_staging_name.push(format!(".{}.tmp", std::process::id()));
    let marker_staging = std::path::PathBuf::from(marker_staging_name);
    let mut marker_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker_staging)
        .expect("create crash marker staging");
    marker_file
        .write_all(WRITER_CRASH_MARKER_CONTENT)
        .expect("write crash marker");
    marker_file.sync_all().expect("sync crash marker");
    drop(marker_file);
    fs::rename(&marker_staging, &marker).expect("publish crash marker");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::park_timeout(Duration::from_millis(250));
    }
    std::process::exit(WRITER_CRASH_HELPER_TIMEOUT_EXIT);
}

#[test]
fn default_indexing_honors_the_cross_process_writer_lock() {
    let workspace = tempdir().expect("workspace");
    fs::write(
        workspace.path().join("main.c"),
        "int default_writer_probe(void) { return 1; }\n",
    )
    .expect("source");
    let index_directory =
        default_index_directory(workspace.path()).expect("default index directory");
    let _cleanup = TestIndexDirectory(index_directory.clone());
    let logical_destination = index_directory.join("index.sqlite");
    let _lock = IndexWriterLock::acquire(&logical_destination).expect("parent writer lock");

    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("indexer::tests::writer_lock::default_index_writer_lock_child")
        .arg("--nocapture")
        .env(DEFAULT_WRITER_CHILD_ROOT_ENV, workspace.path())
        .output()
        .expect("run competing default writer");

    assert!(
        output.status.success(),
        "default indexer bypassed the cross-process writer lock\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn process_termination_releases_the_default_index_writer_lock() {
    let workspace = tempdir().expect("workspace");
    let marker = workspace.path().join("writer-lock.marker");
    let index_directory =
        default_index_directory(workspace.path()).expect("default index directory");
    let _cleanup = TestIndexDirectory(index_directory.clone());
    let logical_destination = index_directory.join("index.sqlite");
    let mut child = ChildGuard(Some(
        Command::new(std::env::current_exe().expect("current test executable"))
            .arg("--exact")
            .arg("indexer::tests::writer_lock::index_writer_lock_crash_child")
            .arg("--nocapture")
            .env(WRITER_CRASH_CHILD_ROOT_ENV, workspace.path())
            .env(WRITER_CRASH_MARKER_ENV, &marker)
            .spawn()
            .expect("spawn lock holder"),
    ));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut early_exit = None;
    let mut marker_ready = false;
    while !marker_ready && Instant::now() < deadline {
        marker_ready =
            fs::read(&marker).is_ok_and(|content| content == WRITER_CRASH_MARKER_CONTENT);
        if let Some(status) = child.child_mut().try_wait().expect("poll lock holder") {
            early_exit = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker_ready,
        "lock holder did not atomically publish its synced marker before timeout; exit={early_exit:?}"
    );
    assert_eq!(
        fs::read(&marker).expect("read crash marker"),
        WRITER_CRASH_MARKER_CONTENT
    );

    child
        .terminate()
        .expect("terminate and reap the writer lock holder");
    IndexWriterLock::acquire(&logical_destination)
        .expect("process termination must release the writer lock");
}
