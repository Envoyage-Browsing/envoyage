//! A hard memory ceiling for the local Chromium process tree.
//!
//! Chromium is multi-process, so guarding only the launcher PID is not enough.
//! Envoyage launches Chromium into its own process group; this module sums RSS
//! for that exact group and terminates only that group if it crosses the limit.

use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

const DEFAULT_BROWSER_MEMORY_LIMIT_MIB: u64 = 4096;
const MIN_BROWSER_MEMORY_LIMIT_MIB: u64 = 256;
const MAX_BROWSER_MEMORY_LIMIT_MIB: u64 = 131_072;
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const TERM_GRACE: Duration = Duration::from_millis(500);

pub(crate) struct BrowserMemoryGuard {
    stop_tx: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl BrowserMemoryGuard {
    pub(crate) fn spawn(process_group: u32) -> Self {
        let limit_mib = configured_memory_limit_mib();
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name(format!("envoyage-memory-{process_group}"))
            .spawn(move || loop {
                match stop_rx.recv_timeout(WATCH_INTERVAL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let Some(rss_kib) = process_group_rss_kib(process_group) else {
                    continue;
                };
                if rss_kib <= limit_mib.saturating_mul(1024) {
                    continue;
                }
                eprintln!(
                    "envoyage: Chromium process group {process_group} used {:.1} MiB, above the \
                     {limit_mib} MiB limit; closing that browser group",
                    rss_kib as f64 / 1024.0
                );
                terminate_process_group(process_group);
                break;
            })
            .ok();
        Self {
            stop_tx: Some(stop_tx),
            handle,
        }
    }
}

impl Drop for BrowserMemoryGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn configured_memory_limit_mib() -> u64 {
    let raw = std::env::var("ENVOYAGE_BROWSER_MEMORY_LIMIT_MB").ok();
    normalize_memory_limit_mib(raw.as_deref())
}

fn normalize_memory_limit_mib(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_BROWSER_MEMORY_LIMIT_MIB)
        .clamp(MIN_BROWSER_MEMORY_LIMIT_MIB, MAX_BROWSER_MEMORY_LIMIT_MIB)
}

fn process_group_rss_kib(process_group: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-axo", "pgid=,rss="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_process_group_rss_kib(
        &String::from_utf8_lossy(&output.stdout),
        process_group,
    ))
}

fn parse_process_group_rss_kib(output: &str, process_group: u32) -> u64 {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pgid = fields.next()?.parse::<u32>().ok()?;
            let rss_kib = fields.next()?.parse::<u64>().ok()?;
            (pgid == process_group).then_some(rss_kib)
        })
        .sum()
}

fn terminate_process_group(process_group: u32) {
    let pgid = process_group as i32;
    // SAFETY: BrowserSession created this exact process group for its child.
    unsafe { nix::libc::kill(-pgid, nix::libc::SIGTERM) };
    std::thread::sleep(TERM_GRACE);
    // Probe the group, not only its leader: helpers may outlive the launcher.
    if unsafe { nix::libc::kill(-pgid, 0) } == 0 {
        // SAFETY: same exact Envoyage-owned process group as above.
        unsafe { nix::libc::kill(-pgid, nix::libc::SIGKILL) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_sum_includes_only_the_owned_process_group() {
        let ps = " 41 100\n 42 9000\n 41 250\nmalformed\n 41 nope\n";
        assert_eq!(parse_process_group_rss_kib(ps, 41), 350);
        assert_eq!(parse_process_group_rss_kib(ps, 42), 9000);
        assert_eq!(parse_process_group_rss_kib(ps, 99), 0);
    }

    #[test]
    fn configured_limit_is_always_bounded() {
        assert_eq!(normalize_memory_limit_mib(None), 4096);
        assert_eq!(normalize_memory_limit_mib(Some("bad")), 4096);
        assert_eq!(normalize_memory_limit_mib(Some("0")), 4096);
        assert_eq!(normalize_memory_limit_mib(Some("1")), 256);
        assert_eq!(normalize_memory_limit_mib(Some("8192")), 8192);
        assert_eq!(normalize_memory_limit_mib(Some("999999")), 131_072);
    }

    #[test]
    fn termination_is_limited_to_the_owned_process_group() {
        use std::os::unix::process::CommandExt as _;

        let mut owned = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn owned test process");
        let mut unrelated = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn unrelated test process");

        terminate_process_group(owned.id());
        let _ = owned.wait();
        assert!(
            unrelated
                .try_wait()
                .expect("probe unrelated process")
                .is_none(),
            "terminating the owned group must not touch another process group"
        );

        let _ = unrelated.kill();
        let _ = unrelated.wait();
    }
}
