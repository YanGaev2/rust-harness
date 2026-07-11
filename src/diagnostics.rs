use std::fs;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticsReport {
    pub process: ProcessDiagnostics,
    pub binary: BinaryDiagnostics,
    pub limits: DiagnosticsLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessDiagnostics {
    pub pid: u32,
    pub rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within_limit: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BinaryDiagnostics {
    pub path: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within_limit: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticsLimits {
    pub within_limits: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsOptions {
    pub binary_path: PathBuf,
    pub max_binary_bytes: Option<u64>,
    pub max_rss_bytes: Option<u64>,
}

pub fn collect(options: DiagnosticsOptions) -> Result<DiagnosticsReport, std::io::Error> {
    let size_bytes = fs::metadata(&options.binary_path)?.len();
    let rss_bytes = current_rss_bytes();
    let binary_within_limit = options
        .max_binary_bytes
        .map(|max_bytes| size_bytes <= max_bytes);
    let rss_within_limit = options
        .max_rss_bytes
        .and_then(|max_bytes| rss_bytes.map(|rss_bytes| rss_bytes <= max_bytes));
    let within_limits = binary_within_limit.unwrap_or(true) && rss_within_limit.unwrap_or(true);

    Ok(DiagnosticsReport {
        process: ProcessDiagnostics {
            pid: std::process::id(),
            rss_bytes,
            max_rss_bytes: options.max_rss_bytes,
            within_limit: rss_within_limit,
        },
        binary: BinaryDiagnostics {
            path: options.binary_path.display().to_string(),
            size_bytes,
            max_bytes: options.max_binary_bytes,
            within_limit: binary_within_limit,
        },
        limits: DiagnosticsLimits { within_limits },
    })
}

#[cfg(windows)]
fn current_rss_bytes() -> Option<u64> {
    use std::ffi::c_void;
    use std::mem;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCounters {
        cb: mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };

    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            mem::size_of::<ProcessMemoryCounters>() as u32,
        )
    };

    (ok != 0).then_some(counters.working_set_size as u64)
}

#[cfg(all(unix, not(windows)))]
fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let vmrss = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?;
    Some(vmrss * 1024)
}

#[cfg(not(any(windows, unix)))]
fn current_rss_bytes() -> Option<u64> {
    None
}
