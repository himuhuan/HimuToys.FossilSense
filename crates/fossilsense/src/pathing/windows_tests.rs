use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use tempfile::tempdir;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

use super::{publish_index_in_directory, resolve_active_index, ExplicitIndexPublication};

struct DeleteBlockingHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for DeleteBlockingHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn open_without_delete_sharing(path: &Path) -> DeleteBlockingHandle {
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        handle,
        INVALID_HANDLE_VALUE,
        "failed to open delete-blocking handle for {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    DeleteBlockingHandle(handle)
}

#[test]
fn explicit_publication_preserves_destination_and_discards_blocked_staging() {
    let dir = tempdir().expect("tempdir");
    let destination = dir.path().join("explicit.sqlite");
    fs::write(&destination, b"old").expect("old destination");
    let blocker = open_without_delete_sharing(&destination);

    let publication = ExplicitIndexPublication::new(destination.clone()).expect("publication");
    let staging = publication.staging_path().to_path_buf();
    fs::write(&staging, b"blocked-new").expect("new staging");
    let error = publication
        .publish()
        .expect_err("open destination handle must block atomic replacement");

    assert!(
        error.to_string().contains("failed to atomically replace"),
        "unexpected publication error: {error:#}"
    );
    assert_eq!(fs::read(&destination).expect("old destination"), b"old");
    assert!(
        !staging.exists(),
        "failed explicit staging must be reclaimed"
    );

    drop(blocker);
    let retry = ExplicitIndexPublication::new(destination.clone()).expect("retry publication");
    fs::write(retry.staging_path(), b"retry-new").expect("retry staging");
    retry.publish().expect("retry after blocker closes");
    assert_eq!(
        fs::read(destination).expect("retried destination"),
        b"retry-new"
    );
}

#[test]
fn manifest_replacement_failure_reclaims_unpublished_generation() {
    let dir = tempdir().expect("tempdir");
    let first_staging = dir.path().join("index-build-first.sqlite");
    fs::write(&first_staging, b"first").expect("first staging");
    let first = publish_index_in_directory(dir.path(), &first_staging, 1).expect("publish first");
    let manifest = dir.path().join("active-index");
    let blocker = open_without_delete_sharing(&manifest);

    let second_staging = dir.path().join("index-build-second.sqlite");
    fs::write(&second_staging, b"second").expect("second staging");
    let error = publish_index_in_directory(dir.path(), &second_staging, 2)
        .expect_err("open manifest handle must block atomic replacement");

    assert!(
        error.to_string().contains("failed to atomically replace"),
        "unexpected publication error: {error:#}"
    );
    assert_eq!(
        resolve_active_index(dir.path()).expect("old active generation"),
        first
    );
    assert_eq!(fs::read(&first).expect("old generation"), b"first");
    assert!(
        !dir.path().join("index-g2-second.sqlite").exists(),
        "unpublished sealed generation must be reclaimed"
    );
    assert!(
        !dir.path().join("active-index-second.tmp").exists(),
        "failed manifest staging file must be reclaimed"
    );

    drop(blocker);
    let retry_staging = dir.path().join("index-build-retry.sqlite");
    fs::write(&retry_staging, b"retry").expect("retry staging");
    let retry =
        publish_index_in_directory(dir.path(), &retry_staging, 2).expect("retry publication");
    assert_eq!(resolve_active_index(dir.path()).unwrap(), retry);
    assert_eq!(fs::read(retry).unwrap(), b"retry");
}
