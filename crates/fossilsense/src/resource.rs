//! Cross-platform process memory and index disk usage reporting.
//!
//! The protocol-neutral collectors are consumed by the LSP adapter and by
//! large-workspace memory gates. Failures return 0 rather than panicking.

use std::path::{Path, PathBuf};

use crate::pathing;

/// Best-effort current process memory in bytes.
///
/// Returns 0 on platforms without a working collector rather than panicking,
/// so the LSP server never crashes on resource reporting.
pub fn current_process_memory_bytes() -> u64 {
    #[cfg(windows)]
    {
        windows_private_bytes()
    }
    #[cfg(target_os = "linux")]
    {
        linux_rss_bytes()
    }
    #[cfg(target_os = "macos")]
    {
        macos_resident_bytes()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

#[cfg(windows)]
fn windows_private_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let loaded = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if loaded == 0 {
        return 0;
    }
    counters.PrivateUsage as u64
}

#[cfg(target_os = "linux")]
fn linux_rss_bytes() -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // "VmRSS:\t      1234 kB"
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "kB" {
                if let Ok(kb) = parts[0].parse::<u64>() {
                    return kb.saturating_mul(1024);
                }
            }
            break;
        }
    }
    0
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MachTaskBasicInfo {
    virtual_size: u64,
    resident_size: u64,
    user_time: u64,
    system_time: u64,
    policy: i32,
    suspend_count: i32,
}

#[cfg(target_os = "macos")]
extern "C" {
    #[link_name = "mach_task_self_"]
    static MACH_TASK_SELF: u32;
    fn task_info(
        target_task: u32,
        flavor: u32,
        task_info_out: *mut MachTaskBasicInfo,
        task_info_count: *mut u32,
    ) -> i32;
}

#[cfg(target_os = "macos")]
fn macos_resident_bytes() -> u64 {
    // MACH_TASK_BASIC_INFO flavor = 20; count = sizeof(MachTaskBasicInfo) / 4.
    // The layout and flavor ID have been stable since macOS 10.4.
    const MACH_TASK_BASIC_INFO: u32 = 20;
    const MACH_TASK_BASIC_INFO_COUNT: u32 = std::mem::size_of::<MachTaskBasicInfo>() as u32 / 4;
    let mut info = MachTaskBasicInfo {
        virtual_size: 0,
        resident_size: 0,
        user_time: 0,
        system_time: 0,
        policy: 0,
        suspend_count: 0,
    };
    let mut count = MACH_TASK_BASIC_INFO_COUNT;
    let kr = unsafe {
        task_info(
            MACH_TASK_SELF,
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _,
            &mut count as *mut _,
        )
    };
    if kr != 0 {
        return 0;
    }
    info.resident_size
}

/// Sum file sizes across all index directories for the given workspace roots.
///
/// Includes active generations, WAL/SHM sidecars, staging files, and the
/// completion history JSON — anything the user would consider "disk used by
/// the FossilSense index". Returns 0 if no directory can be resolved.
pub fn index_directory_disk_bytes(roots: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    for root in roots {
        let Ok(directory) = pathing::default_index_directory(root) else {
            continue;
        };
        total = total.saturating_add(directory_size(&directory));
    }
    total
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path()));
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn current_process_memory_bytes_returns_nonzero_on_supported_platforms() {
        let bytes = current_process_memory_bytes();
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        assert!(
            bytes > 0,
            "memory collector should return a positive value on this platform"
        );
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        assert_eq!(bytes, 0);
    }

    #[test]
    fn directory_size_sums_files_and_subdirectories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir sub");

        let mut f1 = std::fs::File::create(dir.path().join("a.sqlite")).expect("a");
        f1.write_all(&[0u8; 1024]).expect("write a");
        f1.sync_all().expect("sync a");
        drop(f1);

        let mut f2 = std::fs::File::create(sub.join("b.sqlite-wal")).expect("b");
        f2.write_all(&[0u8; 2048]).expect("write b");
        f2.sync_all().expect("sync b");
        drop(f2);

        let size = directory_size(dir.path());
        // Filesystem block allocation may round up; assert the lower bound.
        assert!(
            size >= 3072,
            "directory_size should sum file sizes: got {size}"
        );
    }

    #[test]
    fn directory_size_returns_zero_for_missing_directory() {
        let missing = Path::new("/this/path/does/not/exist/abc123");
        assert_eq!(directory_size(missing), 0);
    }
}
