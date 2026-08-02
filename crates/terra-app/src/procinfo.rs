//! Which program is *actually* running in a tab.
//!
//! A tab's `TerminalBackend` only knows the pid of the shell it spawned. That
//! is rarely what the user is looking at: type `claude` into zsh and the thing
//! on screen is a child of that shell, not the shell. Since terra keys its
//! right-to-left reordering off a per-application compatibility table, "what is
//! on screen" has to be answered from the process tree, and answered often —
//! the foreground program changes whenever a command starts or exits, with no
//! event we could subscribe to.
//!
//! So this module is built around one constraint: it is polled from the UI
//! thread every few hundred milliseconds. That rules out spawning `ps`, and it
//! rules out crates like `sysinfo` that walk `/proc`-equivalents and allocate
//! per process. What is left is a single `sysctl(KERN_PROC_ALL)`, which hands
//! back the whole process table in one copy — a few hundred kilobytes, no
//! syscall per process.
//!
//! The second constraint is that it must never panic. A wrong answer here costs
//! a mis-rendered line of Hebrew; a panic costs the window. Every failure mode
//! — sysctl erroring, the table shrinking mid-call, a pid that exited between
//! the poll and the read, a corrupt parent chain — funnels into `None`, which
//! callers read as "no opinion" and not as an error.
//!
//! Everything platform-specific is `cfg`-gated; other platforms get a stub that
//! always returns `None`, so the caller never has to branch.

/// How far down the child chain we are willing to walk.
///
/// Real nesting is 2–4 deep (shell → command → its helper). The cap exists
/// purely so that a parent-pointer *cycle* — which cannot happen in a healthy
/// kernel table, but can happen in one we read while it was being mutated —
/// terminates instead of spinning the UI thread forever.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAX_DEPTH: usize = 32;

/// The command currently running in the tab whose shell has pid `shell_pid`,
/// as a lowercased basename (e.g. "claude", "codex", "zsh").
///
/// `None` when it cannot be determined, which callers must treat as "no
/// opinion" rather than an error.
#[cfg(target_os = "macos")]
pub fn foreground_command(shell_pid: u32) -> Option<String> {
    let table = process_table()?;

    // A negative pid is not a pid; it means we are looking at a slot the kernel
    // did not fill, so drop the row rather than reasoning about it.
    let edges: Vec<(u32, u32)> = table
        .iter()
        .filter(|p| p.p_pid >= 0 && p.e_ppid >= 0)
        .map(|p| (p.p_pid as u32, p.e_ppid as u32))
        .collect();

    let target = innermost_pid(&edges, shell_pid)?;
    let proc = table
        .iter()
        .find(|p| p.p_pid >= 0 && p.p_pid as u32 == target)?;
    command_name(&proc.p_comm)
}

#[cfg(not(target_os = "macos"))]
pub fn foreground_command(_shell_pid: u32) -> Option<String> {
    None
}

/// The three fields of `struct kinfo_proc` we care about, at their real
/// offsets, with everything else declared as opaque padding.
///
/// `libc` ships `kinfo_proc` for the BSDs but not for Apple, so the layout has
/// to live here. Transcribing the full `extern_proc`/`eproc` pair would mean
/// ~50 fields of kernel pointer types we would never read, each an opportunity
/// to get a size wrong silently. Padding is the smaller surface: three offsets
/// and one total size, every one of them checked at compile time below, and
/// cross-checked at runtime by a test that reads our own process's `e_ppid` and
/// compares it against `getppid`.
///
/// The offsets are ABI, not implementation detail — `kinfo_proc` is a sysctl
/// output struct, so its layout is frozen for the same reason a wire format is.
/// They are identical on x86_64 and arm64 macOS. Derivation, for the reader who
/// wants to check the arithmetic against `<sys/sysctl.h>`:
///
/// - `extern_proc` starts with a 16-byte union, two pointers, an `int` and a
///   `char`, so `p_pid` lands at 40 and `p_comm` at 243, and the struct ends at
///   296.
/// - `eproc` reaches `e_ppid` after two pointers, `_pcred` (104), `_ucred` (76,
///   padded to 80) and `vmspace` (64) — 264 in, i.e. 560 absolute.
/// - The whole thing is 648 bytes, which is also the stride `sysctl` writes at.
#[cfg(target_os = "macos")]
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct KinfoProc {
    /// `kp_proc` up to but excluding `p_pid`.
    _head: [u8; 40],
    p_pid: libc::pid_t,
    /// `p_oppid` through `p_nice`.
    _mid: [u8; 199],
    /// `p_comm`, `[c_char; MAXCOMLEN + 1]` — NUL-*padded*, see `command_name`.
    p_comm: [u8; libc::MAXCOMLEN + 1],
    /// The tail of `kp_proc` plus `kp_eproc` up to but excluding `e_ppid`.
    _tail: [u8; 300],
    e_ppid: libc::pid_t,
    /// The rest of `kp_eproc`, which we never read.
    _rest: [u8; 84],
}

#[cfg(target_os = "macos")]
const _: () = {
    use std::mem::{offset_of, size_of};
    // If any of these ever fire, the padding above is wrong and every field
    // read from the table would be garbage — fail the build, not the user.
    assert!(offset_of!(KinfoProc, p_pid) == 40);
    assert!(offset_of!(KinfoProc, p_comm) == 243);
    assert!(offset_of!(KinfoProc, e_ppid) == 560);
    assert!(size_of::<KinfoProc>() == 648);
};

/// The whole process table, in one `sysctl` round trip.
///
/// `sysctl` is asked for the size first and for the data second, which is
/// inherently racy: processes fork and exit in between, and the kernel answers
/// the second call with `ENOMEM` when the table outgrew the buffer we sized
/// from the first. The usual fix is to over-allocate and retry a bounded number
/// of times — bounded because a machine forking fast enough to lose several
/// races in a row is a machine where the answer does not matter, and we would
/// rather return `None` than keep the UI thread in a loop.
#[cfg(target_os = "macos")]
fn process_table() -> Option<Vec<KinfoProc>> {
    use std::mem::size_of;

    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0];
    let entry = size_of::<KinfoProc>();

    for _ in 0..4 {
        // Pass 1: how big is the table right now?
        let mut len: libc::size_t = 0;
        // SAFETY: `mib` is a valid four-element MIB for KERN_PROC_ALL, and a
        // null data pointer is the documented way to ask only for the size.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }

        // Pass 2, with slack: processes started since pass 1 still fit, so the
        // common case costs one retry fewer.
        let capacity = len / entry + 32;
        let mut buf: Vec<KinfoProc> = Vec::with_capacity(capacity);
        let mut len = capacity * entry;
        // SAFETY: the buffer has room for `len` bytes by construction, and the
        // kernel writes whole `kinfo_proc` records into it. Nothing reads `buf`
        // before `set_len` below marks the written prefix as initialised.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            // ENOMEM means "the table grew again"; anything else is fatal and
            // retrying would just repeat it. Either way `buf` is dropped with
            // length 0, so no uninitialised element is ever touched.
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOMEM) {
                continue;
            }
            return None;
        }

        // A short write is fine, a long one would mean trusting the kernel past
        // our allocation — clamp rather than believe it.
        let count = (len / entry).min(capacity);
        if count == 0 {
            return None;
        }
        // SAFETY: `sysctl` reported `len` bytes written, i.e. `count` fully
        // initialised records, and `count <= capacity`.
        unsafe { buf.set_len(count) };
        return Some(buf);
    }
    None
}

/// The pid of the innermost descendant of `shell_pid` — the program the user is
/// actually typing at — or `shell_pid` itself when the shell has no children.
///
/// "Innermost" is depth first and recency second: the deepest generation wins,
/// and within that generation the highest pid, which on a wrapping-but-monotonic
/// pid allocator is the most recently started sibling. That heuristic is what
/// makes `zsh → claude → some helper` report the helper's ancestor chain
/// correctly rather than reporting `zsh`.
///
/// `None` means `shell_pid` is not in the table at all — the tab's shell has
/// exited, and any name we produced would be a lie.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn innermost_pid(edges: &[(u32, u32)], shell_pid: u32) -> Option<u32> {
    if !edges.iter().any(|&(pid, _)| pid == shell_pid) {
        return None;
    }

    let mut best = shell_pid;
    let mut frontier = vec![shell_pid];
    // Visited is what actually breaks cycles; MAX_DEPTH is the backstop for a
    // table so mangled that it produces fresh pids forever.
    let mut seen = vec![shell_pid];

    for _ in 0..MAX_DEPTH {
        let mut next: Vec<u32> = Vec::new();
        for &(pid, ppid) in edges {
            // `pid != ppid` skips the self-parenting slot that a torn read can
            // produce, and pid 0, which is its own parent by convention.
            if pid != ppid && frontier.contains(&ppid) && !seen.contains(&pid) {
                seen.push(pid);
                next.push(pid);
            }
        }
        let Some(&deepest) = next.iter().max() else {
            break;
        };
        best = deepest;
        frontier = next;
    }
    Some(best)
}

/// Normalise a raw `p_comm` byte buffer into a comparable command name.
///
/// Three things make this less trivial than a `CStr::from_bytes` call. The
/// field is a fixed-size array that is only NUL-*padded*, so a name that fills
/// it exactly has no terminator at all. A login shell is spelled `-zsh`, and the
/// leading dash is a convention, not part of the name. And the kernel copies
/// whatever bytes `execve` was handed, so the contents need not be UTF-8 — which
/// is a reason to replace invalid sequences, never to panic.
///
/// The result is lowercased so callers can compare against a table of
/// lowercase keys directly. An empty or all-NUL buffer yields `None`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn command_name(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name = String::from_utf8_lossy(&raw[..end]);
    let name = name.trim().trim_start_matches('-');
    if name.is_empty() {
        return None;
    }
    Some(name.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nul_terminated_name_stops_at_the_terminator() {
        assert_eq!(command_name(b"zsh\0\0\0\0\0"), Some("zsh".into()));
    }

    #[test]
    fn a_name_that_fills_the_buffer_is_not_treated_as_unterminated_garbage() {
        // MAXCOMLEN + 1 bytes of name and nowhere to put a NUL.
        let raw = b"abcdefghijklmnopq";
        assert_eq!(command_name(raw), Some("abcdefghijklmnopq".into()));
    }

    #[test]
    fn an_all_nul_buffer_has_no_command_name() {
        assert_eq!(command_name(&[0u8; 17]), None);
        assert_eq!(command_name(&[]), None);
    }

    #[test]
    fn a_login_shell_loses_its_leading_dash() {
        assert_eq!(command_name(b"-zsh\0"), Some("zsh".into()));
        assert_eq!(command_name(b"-\0"), None);
    }

    #[test]
    fn names_are_lowercased_so_the_compatibility_table_can_use_one_spelling() {
        assert_eq!(command_name(b"Claude\0"), Some("claude".into()));
        assert_eq!(command_name(b"NVIM\0"), Some("nvim".into()));
    }

    #[test]
    fn invalid_utf8_is_replaced_rather_than_panicking() {
        let name = command_name(&[b'z', 0xff, 0xfe, b'h', 0]);
        assert!(name.is_some());
        assert!(name.unwrap().starts_with('z'));
    }

    #[test]
    fn a_shell_with_no_children_names_itself() {
        let edges = [(1u32, 0u32), (500, 1), (501, 1)];
        assert_eq!(innermost_pid(&edges, 500), Some(500));
    }

    #[test]
    fn the_deepest_descendant_wins_over_a_shallower_sibling() {
        // 500 ─┬─ 600 ── 700   (depth 2)
        //      └─ 900          (depth 1, higher pid)
        let edges = [(500u32, 1u32), (600, 500), (700, 600), (900, 500)];
        assert_eq!(innermost_pid(&edges, 500), Some(700));
    }

    #[test]
    fn the_most_recently_started_sibling_breaks_a_tie_at_equal_depth() {
        let edges = [(500u32, 1u32), (600, 500), (900, 500)];
        assert_eq!(innermost_pid(&edges, 500), Some(900));
    }

    #[test]
    fn a_pid_missing_from_the_table_yields_no_opinion() {
        let edges = [(1u32, 0u32), (500, 1)];
        assert_eq!(innermost_pid(&edges, 4242), None);
    }

    #[test]
    fn a_cycle_in_the_parent_chain_terminates_instead_of_hanging() {
        // 500 → 600 → 700 → 500: a table this corrupt cannot come from a
        // healthy kernel, but a torn read could fake it.
        let edges = [(500u32, 700u32), (600, 500), (700, 600)];
        assert!(innermost_pid(&edges, 500).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_hand_written_kinfo_proc_layout_agrees_with_the_kernel() {
        // The compile-time asserts prove the offsets are where we think; this
        // proves they are where the *kernel* thinks, by checking our own row
        // against two values we already know from libc.
        let table = process_table().expect("the process table is always readable");
        let me = std::process::id() as libc::pid_t;
        let row = table
            .iter()
            .find(|p| p.p_pid == me)
            .expect("our own process is in its own process table");
        // SAFETY: `getppid` takes no arguments and cannot fail.
        assert_eq!(row.e_ppid, unsafe { libc::getppid() });
        assert!(command_name(&row.p_comm).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn our_own_pid_resolves_through_the_real_process_table() {
        let name = foreground_command(std::process::id())
            .expect("the test binary's own pid is always in the table");
        assert!(!name.is_empty());
        assert_eq!(name, name.to_lowercase());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_implausible_pid_yields_none_without_panicking() {
        // Note pid 0 is deliberately not tested: `kernel_task` really is in the
        // table and really is the ancestor of everything, so it is a valid
        // query, just not a meaningful one.
        assert_eq!(foreground_command(u32::MAX), None);
        assert_eq!(foreground_command(u32::MAX - 1), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_child_process_is_reported_instead_of_its_parent() {
        // This is the behaviour the feature exists for: the shell is not the
        // interesting process once it has spawned something.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .expect("/bin/sleep exists on every macOS install");

        // `spawn` returns once fork+exec is under way; the exec that renames the
        // process from the test binary to `sleep` can land a moment later.
        let mut name = None;
        for _ in 0..50 {
            name = foreground_command(std::process::id());
            if name.as_deref() == Some("sleep") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name.as_deref(), Some("sleep"));
    }
}
