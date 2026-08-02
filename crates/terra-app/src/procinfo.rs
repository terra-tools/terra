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
//! rules out crates like `sysinfo`, which allocate a rich record per process
//! and re-stat far more than a pid and a parent.
//!
//! The shape is the same on all three platforms and only the middle step
//! differs: enumerate `(pid, ppid, name)` for every process, walk down from the
//! tab's shell with [`innermost_pid`], then name the pid we landed on with
//! [`command_name`]. Only the enumeration is `cfg`-gated, so the tree walk and
//! the naming rules — the parts with the interesting edge cases — are one
//! implementation tested everywhere.
//!
//! A name is not always enough — `codex` is a node script and the kernel calls
//! it `node` — so the pid the walk lands on is also asked for its arguments
//! ([`process_argv`], macOS only for now), and the pair travels as a
//! [`Foreground`]. That lookup follows the same per-platform adapter shape:
//! a `cfg`-gated call around a pure parser, with an empty answer everywhere
//! else.
//!
//! - **macOS** — one `sysctl(KERN_PROC_ALL)`, which hands back the whole
//!   process table in a single copy: a few hundred kilobytes, no syscall per
//!   process.
//! - **Windows** — one `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)`, likewise
//!   a single kernel-side snapshot walked with `Process32NextW`.
//! - **Linux** — `/proc`, which has no whole-table call: one `read` of
//!   `/proc/<pid>/stat` per process, a few hundred of them. That is more
//!   syscalls than the other two but each is a page out of a virtual file with
//!   no I/O behind it, and at two polls a second it does not register.
//!
//! The second constraint is that it must never panic. A wrong answer here costs
//! a mis-rendered line of Hebrew; a panic costs the window. Every failure mode
//! — the enumeration erroring, the table shrinking mid-call, a pid that exited
//! between the poll and the read, a corrupt parent chain — funnels into `None`,
//! which callers read as "no opinion" and not as an error.
//!
//! Platforms outside the three get a stub that always returns `None`, so the
//! caller never has to branch.

/// How far down the child chain we are willing to walk.
///
/// Real nesting is 2–4 deep (shell → command → its helper). The cap exists
/// purely so that a parent-pointer *cycle* — which cannot happen in a healthy
/// kernel table, but can happen in one we read while it was being mutated —
/// terminates instead of spinning the UI thread forever.
// Dead only on a platform with no process enumeration below; the three that
// have one all route through it.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
const MAX_DEPTH: usize = 32;

/// What is running in a tab: the innermost process's name, plus the first few
/// entries of its argv.
///
/// The name alone is not enough to recognise a wrapped CLI. `codex` is a node
/// script, so the kernel calls its process `node` and a tab running it looks
/// exactly like a tab running a REPL. The argv is where the difference lives —
/// `node /…/bin/codex` — so it is carried alongside, and the icon layer decides
/// what to make of it.
///
/// `argv` is best-effort and frequently empty (an unsupported platform, a
/// process that exited, a buffer that would not parse). Nothing may depend on
/// it being populated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Foreground {
    /// Lowercased basename from the process table, e.g. "claude", "node".
    pub name: String,
    /// Executable paths and arguments for the foreground chain, innermost
    /// process first: [`MAX_ARGV`] entries each, for up to [`MAX_CHAIN`]
    /// processes ([`chain_to_shell`]). Raw — not lowercased, not reduced to
    /// basenames — because a *path* is often where the identity is.
    pub argv: Vec<String>,
}

/// How many argv entries past the executable path are worth keeping.
///
/// Two: an interpreter's argv is `<interp> <script> [subcommand]`, and the
/// identity of the tab is in the script or, for `npx foo`-shaped launchers, one
/// word after it. Everything beyond that is the program's own arguments, where a
/// path is as likely to name a *file being edited* as the editor.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAX_ARGV: usize = 2;

/// The command currently running in the tab whose shell has pid `shell_pid`,
/// as a lowercased basename (e.g. "claude", "codex", "zsh").
///
/// `None` when it cannot be determined, which callers must treat as "no
/// opinion" rather than an error.
pub fn foreground_command(shell_pid: u32) -> Option<String> {
    Some(foreground_commands(&[shell_pid]).pop()??.name)
}

/// [`foreground_command`] for several shells at once, answering in the order
/// asked and always with exactly `shell_pids.len()` entries.
///
/// The batch form exists because the expensive part is the *snapshot*, not the
/// lookup: one `sysctl` (or one ToolHelp snapshot, or one sweep of `/proc`)
/// hands back the whole machine, and the tab bar wants an answer for every open
/// tab at once. Asking per tab would multiply the only costly step by the
/// number of tabs to re-derive the same table each time.
///
/// Answering from one snapshot is also the more correct thing to do: every
/// tab's answer then describes the same instant, rather than a row of tabs each
/// describing a slightly different one.
///
/// The argv lookup is the one part that cannot come from the shared snapshot —
/// the kernel hands out a process's arguments one pid at a time — so it costs
/// one extra call per *tab*, not per process, and only for the pid the walk
/// landed on.
pub fn foreground_commands(shell_pids: &[u32]) -> Vec<Option<Foreground>> {
    let Some(rows) = snapshot() else {
        return vec![None; shell_pids.len()];
    };
    let edges: Vec<(u32, u32)> = rows.iter().map(|(pid, ppid, _)| (*pid, *ppid)).collect();
    shell_pids
        .iter()
        .map(|shell_pid| {
            let target = innermost_pid(&edges, *shell_pid)?;
            let (_, _, name) = rows.iter().find(|(pid, _, _)| *pid == target)?;
            Some(Foreground {
                name: command_name(name)?,
                argv: chain_to_shell(&edges, target, *shell_pid)
                    .into_iter()
                    .flat_map(process_argv)
                    .collect(),
            })
        })
        .collect()
}

/// The processes between `innermost` and the tab's shell, innermost first and
/// the shell itself excluded.
///
/// The innermost process is the right answer to "what is the user typing at",
/// and the wrong one to "what is this tab". A real `codex` session is
/// `zsh → node → codex → node_repl`: the deepest process is a helper nobody has
/// heard of, and the identity of the tab is one step up. So the argv of the
/// whole chain is collected and the icon layer takes the first entry it
/// recognises, which is innermost-wins with a fallback rather than a new rule.
///
/// Bounded by [`MAX_CHAIN`], and by a self-parent or a missing row, so a table
/// read mid-mutation cannot turn this into a loop.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
fn chain_to_shell(edges: &[(u32, u32)], innermost: u32, shell_pid: u32) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut pid = innermost;
    while pid != shell_pid && chain.len() < MAX_CHAIN {
        chain.push(pid);
        let Some(&(_, ppid)) = edges.iter().find(|&&(p, _)| p == pid) else {
            break;
        };
        if ppid == pid || chain.contains(&ppid) {
            break;
        }
        pid = ppid;
    }
    chain
}

/// How many processes up from the innermost one are worth asking for their
/// arguments. Four covers `node → codex → helper` with room to spare, and caps
/// the per-tab cost of [`process_argv`] at a handful of small `sysctl`s.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
const MAX_CHAIN: usize = 4;

/// The executable path and first [`MAX_ARGV`] arguments of one process.
///
/// `sysctl(KERN_PROCARGS2)` is the only way to read another process's arguments
/// on macOS without spawning `ps`, and unlike `KERN_PROC_ALL` it answers for one
/// pid at a time. It is asked for its size first, because the buffer it wants is
/// bounded by `ARG_MAX` (a megabyte) and allocating that per tab per second to
/// read forty bytes would be silly.
///
/// Every failure — the process exited between the walk and this call, it belongs
/// to another user, the buffer does not parse — is an empty vector. Arguments
/// are a hint, never a requirement.
#[cfg(target_os = "macos")]
fn process_argv(pid: u32) -> Vec<String> {
    /// Refuse to allocate more than this, whatever the kernel says it wants.
    const CAP: libc::size_t = 1 << 20;

    let Ok(pid) = libc::c_int::try_from(pid) else {
        return Vec::new();
    };
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];

    // Pass 1: how much does this process's argument block need?
    let mut len: libc::size_t = 0;
    // SAFETY: `mib` is a valid three-element MIB, and a null data pointer with a
    // real `len` out-parameter is the documented size query.
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
    if rc != 0 || len == 0 || len > CAP {
        return Vec::new();
    }

    // Pass 2. A block that grew past `len` in between fails with ENOMEM, and
    // one missing icon is a better answer than a retry loop on the UI thread.
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` owns `len` writable bytes, and `len` is updated in place
    // with however many the kernel actually wrote.
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
        return Vec::new();
    }
    buf.truncate(len.min(buf.len()));
    parse_procargs2(&buf, MAX_ARGV)
}

/// See [`process_argv`] — no equivalent single call elsewhere, so the icon layer
/// falls back to the process name alone, exactly as it did before argv existed.
#[cfg(not(target_os = "macos"))]
fn process_argv(_pid: u32) -> Vec<String> {
    Vec::new()
}

/// The executable path and up to `max_argv` arguments out of a raw
/// `KERN_PROCARGS2` block.
///
/// The layout is undocumented but stable, and is the reason this is a parser
/// rather than a split: a native-endian `int` argc, then the *executable path*
/// (which is not `argv[0]` and is not counted by argc), then NUL padding of
/// unpredictable length that aligns what follows, then argc NUL-terminated
/// arguments, then — immediately, with no marker — the environment.
///
/// The environment is why `argc` is honoured rather than ignored: without it a
/// process invoked as a bare `node` would keep reading and hand back
/// `TERM=xterm-256color`. Empty fields are skipped, which handles the padding
/// and an empty argument in one rule, and every length is bounded, so a
/// truncated or garbage block yields a short answer instead of a panic or a
/// gigabyte of strings.
///
/// Pure, so the layout is tested on every platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_procargs2(buf: &[u8], max_argv: usize) -> Vec<String> {
    /// Longer than any argument that could name a program. A command line can
    /// carry a whole file in it.
    const MAX_LEN: usize = 1024;

    let mut out = Vec::new();
    if buf.len() < 4 {
        return out;
    }
    let (head, rest) = buf.split_at(4);
    let argc = u32::from_ne_bytes([head[0], head[1], head[2], head[3]]) as usize;

    let take = |field: &[u8]| {
        let end = field.len().min(MAX_LEN);
        String::from_utf8_lossy(&field[..end]).trim().to_string()
    };

    let mut fields = rest.split(|&b| b == 0);
    // The executable path, which sits before argv and outside argc's count.
    match fields.next() {
        Some(exec) if !exec.is_empty() => out.push(take(exec)),
        _ => return out,
    }
    let wanted = max_argv.min(argc);
    for field in fields {
        if out.len() > wanted {
            break;
        }
        if field.is_empty() {
            continue;
        }
        out.push(take(field));
    }
    out
}

/// `(pid, ppid, raw name)` for every process on the machine, from one kernel
/// snapshot.
///
/// The only `cfg`-gated step. Each platform's `process_table` already produces
/// a consistent snapshot in its own shape; this flattens the three shapes into
/// the one the tree walk and [`command_name`] consume, so everything downstream
/// of here is a single implementation tested everywhere.
#[cfg(target_os = "macos")]
fn snapshot() -> Option<Vec<(u32, u32, Vec<u8>)>> {
    let table = process_table()?;
    Some(
        table
            .iter()
            // A negative pid is not a pid; it means we are looking at a slot
            // the kernel did not fill, so drop the row rather than reasoning
            // about it.
            .filter(|p| p.p_pid >= 0 && p.e_ppid >= 0)
            .map(|p| (p.p_pid as u32, p.e_ppid as u32, p.p_comm.to_vec()))
            .collect(),
    )
}

/// See [`snapshot`] — same contract, ToolHelp enumeration.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn snapshot() -> Option<Vec<(u32, u32, Vec<u8>)>> {
    let table = process_table()?;
    Some(
        table
            .into_iter()
            .map(|row| (row.pid, row.ppid, row.name.into_bytes()))
            .collect(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn snapshot() -> Option<Vec<(u32, u32, Vec<u8>)>> {
    None
}

/// One process, reduced to the three things the tree walk needs.
///
/// macOS reads these straight out of its `kinfo_proc` rows and needs no such
/// type; Windows and Linux both have to copy the name out of a kernel buffer
/// anyway, so they materialise it here and share everything downstream.
#[cfg(any(target_os = "windows", target_os = "linux"))]
struct ProcRow {
    pid: u32,
    ppid: u32,
    /// The executable's own name, already stripped of its directory (and, on
    /// Windows, its `.exe`). Still raw otherwise — [`command_name`] does the
    /// trimming, dash-stripping and lowercasing.
    name: String,
}

/// Every process on the machine, from one kernel snapshot.
///
/// `CreateToolhelp32Snapshot` copies the process list into kernel memory in one
/// call and `Process32NextW` then walks that copy, so the whole enumeration
/// sees a consistent table rather than one that shifts under it — the same
/// property `sysctl(KERN_PROC_ALL)` gives on macOS, and the reason neither
/// platform needs the retry loop a per-process API would.
///
/// The snapshot is a kernel handle, so it is wrapped in a guard: every early
/// return below is a leaked handle otherwise, and this runs twice a second for
/// the lifetime of the app.
#[cfg(target_os = "windows")]
fn process_table() -> Option<Vec<ProcRow>> {
    use std::mem::size_of;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    struct Snapshot(HANDLE);
    impl Drop for Snapshot {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `CreateToolhelp32Snapshot` and was
            // checked against `INVALID_HANDLE_VALUE`, so it is a live handle
            // this guard uniquely owns, and it is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    // SAFETY: no arguments to get wrong; the call either yields a handle or
    // `INVALID_HANDLE_VALUE`, which is checked before the guard takes ownership.
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let snapshot = Snapshot(handle);

    // `dwSize` is how the API knows which version of the struct it was handed;
    // leaving it zero makes `Process32FirstW` fail with ERROR_INVALID_PARAMETER.
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    // SAFETY: `snapshot.0` is a live process snapshot and `entry` is a
    // correctly sized, fully initialised `PROCESSENTRY32W` we exclusively own.
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        return None;
    }

    let mut rows = Vec::with_capacity(256);
    loop {
        rows.push(ProcRow {
            pid: entry.th32ProcessID,
            ppid: entry.th32ParentProcessID,
            name: exe_name(&entry.szExeFile),
        });
        // SAFETY: as above; `Process32NextW` returns zero at the end of the
        // list (and on any error), which ends the loop.
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            break;
        }
    }

    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// `PROCESSENTRY32W::szExeFile` — a NUL-terminated UTF-16 buffer — as a name
/// the shared [`command_name`] can finish normalising.
///
/// Two Windows-only adjustments happen here rather than in `command_name`, so
/// the macOS path stays exactly what it was. The buffer is UTF-16 and only
/// NUL-*terminated* within its 260 units, so it is cut at the first NUL and
/// decoded lossily — an unpaired surrogate is a replacement character, never a
/// panic. And the trailing `.exe` is dropped, case-insensitively, so that one
/// `[text.quirks]` table written as `claude` matches `claude.exe` here and
/// `claude` on the other two platforms.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn exe_name(raw: &[u16]) -> String {
    let end = raw.iter().position(|&u| u == 0).unwrap_or(raw.len());
    let name = String::from_utf16_lossy(&raw[..end]);
    match name.len().checked_sub(4) {
        Some(stem) if name[stem..].eq_ignore_ascii_case(".exe") => name[..stem].to_string(),
        _ => name,
    }
}

/// Every process on the machine, one `/proc/<pid>/stat` at a time.
///
/// Linux has no equivalent of the single-copy calls the other two platforms
/// use, so this is genuinely a syscall per process. It is affordable because
/// `/proc` files are generated on read with no block device behind them, and
/// because the poll is twice a second, not per frame.
///
/// A pid that exits mid-walk simply fails its `read` and is skipped: that is
/// the same "the table moved under us" race macOS retries for, and here it
/// costs one missing row rather than a wrong answer.
#[cfg(target_os = "linux")]
fn process_table() -> Option<Vec<ProcRow>> {
    let mut rows = Vec::with_capacity(256);
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        // `/proc` holds `self`, `net`, `sys` and friends alongside the pids;
        // an all-digits name is exactly the process test.
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some((pid, ppid, name)) = parse_stat(pid, &stat) {
            rows.push(ProcRow { pid, ppid, name });
        }
    }
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// `pid`, `ppid` and `comm` out of one `/proc/<pid>/stat` line.
///
/// Splitting on whitespace and indexing field 4 is the obvious implementation
/// and it is wrong: field 2 is the command in parentheses, and a command may
/// contain spaces *and* parentheses (`(tmux: server)`, or any process that
/// chose to `prctl(PR_SET_NAME)` itself something awkward), which shifts every
/// later field by an unpredictable amount. The kernel's own documented
/// workaround is to find the *last* `)` in the line: everything between the
/// first `(` and it is the name, and the fields resume immediately after.
///
/// Like macOS's `p_comm`, this name is truncated by the kernel — to 15 bytes
/// rather than 16 — which is fine for a table keyed on names like `claude` and
/// `nvim`, and is the same limitation the macOS path has always had.
///
/// Pure, so the parenthesis handling is tested on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat(pid: u32, stat: &str) -> Option<(u32, u32, String)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?;
    // What follows the `)` is ` <state> <ppid> …`.
    let mut fields = stat.get(close + 1..)?.split_whitespace();
    let _state = fields.next()?;
    let ppid: u32 = fields.next()?.parse().ok()?;
    Some((pid, ppid, comm.to_string()))
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
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
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
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
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

    /// The tree walk is one implementation for three platforms, so the shape
    /// every enumeration produces — a shell with a real chain under it — has to
    /// resolve the same way regardless of where the edges came from.
    ///
    /// These are the pids and parents a Windows `Process32NextW` walk or a
    /// Linux `/proc` sweep would hand over for `pwsh → claude → node`, in the
    /// arbitrary order the kernel happens to list them in.
    #[test]
    fn a_windows_or_linux_style_edge_list_resolves_the_same_as_a_macos_one() {
        // 4 (System) ─ 1200 (pwsh) ─ 1300 (claude) ─ 1400 (node)
        let edges = [
            (1400u32, 1300u32),
            (4, 0),
            (1200, 4),
            (9999, 4), // an unrelated sibling of the shell, not a descendant
            (1300, 1200),
        ];
        assert_eq!(innermost_pid(&edges, 1200), Some(1400));
        assert_eq!(innermost_pid(&edges, 1300), Some(1400));
        assert_eq!(innermost_pid(&edges, 9999), Some(9999));
    }

    /// Windows names carry an extension the other platforms do not, and one
    /// `[text.quirks]` table has to match on all three — so `.exe` comes off
    /// here, before the shared normalisation, and the macOS path never sees it.
    #[test]
    fn a_windows_executable_name_loses_its_extension_and_its_padding() {
        let wide = |s: &str| {
            let mut buf = [0u16; 260];
            for (slot, unit) in buf.iter_mut().zip(s.encode_utf16()) {
                *slot = unit;
            }
            buf
        };
        assert_eq!(exe_name(&wide("claude.exe")), "claude");
        // Windows filenames are case-insensitive, and installers are not
        // consistent about which case they write.
        assert_eq!(exe_name(&wide("PowerShell.EXE")), "PowerShell");
        // A dot that is not a trailing `.exe` is part of the name.
        assert_eq!(exe_name(&wide("python3.11")), "python3.11");
        assert_eq!(exe_name(&wide("node")), "node");
        // A name that is exactly the extension keeps it: dropping it would
        // leave nothing at all.
        assert_eq!(exe_name(&wide(".exe")), "");
        assert_eq!(exe_name(&[0u16; 260]), "");
        // Downstream, the shared rules still apply.
        assert_eq!(
            command_name(exe_name(&wide("Claude.exe")).as_bytes()),
            Some("claude".into())
        );
    }

    /// An unpaired surrogate is a replacement character, not a panic — the
    /// kernel copies whatever bytes it was handed here just as it does on macOS.
    #[test]
    fn an_ill_formed_windows_name_is_replaced_rather_than_panicking() {
        let mut buf = [0u16; 260];
        buf[0] = u16::from(b'z');
        buf[1] = 0xD800; // a high surrogate with nothing after it
        buf[2] = u16::from(b'h');
        assert!(exe_name(&buf).starts_with('z'));
    }

    /// `/proc/<pid>/stat` cannot be split on whitespace: field 2 is the command
    /// in parentheses, and a command may contain both spaces and parentheses,
    /// which would shift `ppid` to an unpredictable index.
    #[test]
    fn a_proc_stat_line_is_parsed_by_its_last_parenthesis() {
        // The ordinary case: no space, no nesting.
        let plain = "1300 (claude) S 1200 1300 1200 34816 1300 4194304 512 0 0 0";
        assert_eq!(
            parse_stat(1300, plain),
            Some((1300, 1200, "claude".to_string()))
        );

        // The case that breaks the naive parse. Splitting on whitespace would
        // read `server)` as the state and `S` as the ppid.
        let spaced = "700 (tmux: server) S 1 700 700 0 -1 4194304 900 0 0 0";
        assert_eq!(
            parse_stat(700, spaced),
            Some((700, 1, "tmux: server".to_string()))
        );

        // Parentheses inside the name: only the *last* `)` closes it.
        let nested = "800 (weird (name)) R 42 800 800 0 -1 0 1 0 0 0";
        assert_eq!(
            parse_stat(800, nested),
            Some((800, 42, "weird (name)".to_string()))
        );

        // An empty comm is legal and is not a parse failure — `command_name`
        // is what decides it carries no opinion.
        assert_eq!(parse_stat(9, "9 () S 1 9"), Some((9, 1, String::new())));
        assert_eq!(command_name(b""), None);
    }

    /// A truncated, reordered or otherwise unreadable `stat` is one skipped row,
    /// never a panic: `/proc` entries vanish under the reader all the time.
    #[test]
    fn a_malformed_proc_stat_line_yields_no_row() {
        assert_eq!(parse_stat(1, ""), None);
        assert_eq!(parse_stat(1, "1 init S 1"), None); // no parentheses at all
        assert_eq!(parse_stat(1, "1 (init)"), None); // truncated before ppid
        assert_eq!(parse_stat(1, "1 (init) S"), None); // state but no ppid
        assert_eq!(parse_stat(1, "1 (init) S notanumber"), None);
        // A `)` before the `(` cannot delimit a name; `get` returns `None`
        // rather than slicing backwards and panicking.
        assert_eq!(parse_stat(1, "1 )init( S 1"), None);
        // Multi-byte characters must not be sliced through the middle.
        assert_eq!(
            parse_stat(1, "1 (héllo) S 1 1"),
            Some((1, 1, "héllo".to_string()))
        );
    }

    /// The chain is what makes a real `codex` session — whose deepest process
    /// is an anonymous helper — resolvable: the identity is one step up.
    #[test]
    fn the_chain_runs_from_the_innermost_process_up_to_the_shell() {
        // 500 (zsh) → 600 (node) → 700 (codex) → 800 (node_repl)
        let edges = [(500u32, 1u32), (600, 500), (700, 600), (800, 700)];
        let innermost = innermost_pid(&edges, 500).expect("in the table");
        assert_eq!(innermost, 800);
        assert_eq!(chain_to_shell(&edges, innermost, 500), [800, 700, 600]);
        // A shell with nothing in it contributes nothing to ask about.
        assert_eq!(chain_to_shell(&edges, 500, 500), [] as [u32; 0]);
    }

    /// Same guarantee as the tree walk: a table read while it was being mutated
    /// must not become a loop, and the depth is capped regardless.
    #[test]
    fn a_broken_parent_chain_is_bounded_rather_than_followed_forever() {
        let cycle = [(700u32, 800u32), (800, 700)];
        assert!(chain_to_shell(&cycle, 700, 1).len() <= MAX_CHAIN);
        let deep: Vec<(u32, u32)> = (1..40u32).map(|p| (p + 1, p)).collect();
        assert_eq!(chain_to_shell(&deep, 40, 1).len(), MAX_CHAIN);
        // A pid whose row vanished ends the walk where it stands.
        assert_eq!(chain_to_shell(&[], 900, 1), [900]);
    }

    /// A synthetic `KERN_PROCARGS2` block: argc, the executable path, `pad`
    /// NULs of alignment, then the arguments and whatever follows them.
    fn procargs2(argc: u32, exec: &str, pad: usize, tail: &[&str]) -> Vec<u8> {
        let mut buf = argc.to_ne_bytes().to_vec();
        buf.extend_from_slice(exec.as_bytes());
        buf.push(0);
        buf.extend(std::iter::repeat_n(0u8, pad));
        for field in tail {
            buf.extend_from_slice(field.as_bytes());
            buf.push(0);
        }
        buf
    }

    /// The layout the icon layer depends on: the executable path is *not*
    /// `argv[0]` and is not counted by argc, and the padding between them is of
    /// no fixed length.
    #[test]
    fn an_argument_block_yields_the_exec_path_and_the_first_arguments() {
        let buf = procargs2(
            2,
            "/usr/local/bin/node",
            7,
            &["node", "/opt/homebrew/lib/node_modules/codex/bin/codex.js"],
        );
        assert_eq!(
            parse_procargs2(&buf, MAX_ARGV),
            [
                "/usr/local/bin/node",
                "node",
                "/opt/homebrew/lib/node_modules/codex/bin/codex.js"
            ]
        );
    }

    /// argc is what separates the arguments from the environment, which follows
    /// them with no marker of its own. A bare REPL must not come back wearing
    /// its `TERM`.
    #[test]
    fn the_environment_after_the_arguments_is_not_mistaken_for_one() {
        let buf = procargs2(
            1,
            "/usr/bin/node",
            3,
            &["node", "TERM=xterm", "SHELL=/bin/zsh"],
        );
        assert_eq!(parse_procargs2(&buf, MAX_ARGV), ["/usr/bin/node", "node"]);
    }

    /// Two arguments is the whole budget: past the script, a path is as likely
    /// to name the file being edited as the editor.
    #[test]
    fn no_more_arguments_are_kept_than_were_asked_for() {
        let buf = procargs2(9, "/bin/sh", 1, &["sh", "-c", "vim", "notes.md", "extra"]);
        assert_eq!(parse_procargs2(&buf, MAX_ARGV), ["/bin/sh", "sh", "-c"]);
        assert_eq!(parse_procargs2(&buf, 0), ["/bin/sh"]);
    }

    /// Every failure mode is a short answer, never a panic: this is parsing a
    /// kernel buffer for a process that may have exited mid-call.
    #[test]
    fn a_truncated_or_nonsense_argument_block_parses_to_nothing() {
        assert!(parse_procargs2(&[], MAX_ARGV).is_empty());
        assert!(parse_procargs2(&[1, 0, 0], MAX_ARGV).is_empty()); // no room for argc
        assert!(parse_procargs2(&[1, 0, 0, 0], MAX_ARGV).is_empty()); // argc, nothing after
                                                                      // An argc the kernel could never produce must not be believed.
        let huge = procargs2(u32::MAX, "/bin/ls", 1, &["ls", "-l", "/tmp", "and-more"]);
        assert_eq!(parse_procargs2(&huge, MAX_ARGV), ["/bin/ls", "ls", "-l"]);
        // A block that ends mid-argument keeps what was whole.
        let mut cut = procargs2(2, "/bin/ls", 1, &["ls", "-l"]);
        cut.truncate(cut.len() - 1);
        assert_eq!(parse_procargs2(&cut, MAX_ARGV), ["/bin/ls", "ls", "-l"]);
    }

    /// The kernel copies whatever `execve` was handed, so an argument need not
    /// be UTF-8 and need not be short.
    #[test]
    fn an_ill_formed_or_enormous_argument_is_bounded_rather_than_trusted() {
        let mut buf = 1u32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/z\xff\xfeh\0\0");
        buf.extend_from_slice(&vec![b'x'; 4096]);
        buf.push(0);
        let argv = parse_procargs2(&buf, MAX_ARGV);
        assert!(argv[0].starts_with("/bin/z"));
        assert!(argv[1].len() <= 1024, "{} bytes", argv[1].len());
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

    /// The synthetic buffers above prove the layout; this proves the *kernel*
    /// agrees, by reading the arguments of the one process we know the argv of.
    #[cfg(target_os = "macos")]
    #[test]
    fn our_own_arguments_come_back_from_the_kernel() {
        let argv = process_argv(std::process::id());
        assert!(!argv.is_empty(), "no arguments for our own pid");
        // The exec path leads, and it is this test binary.
        assert!(argv[0].contains('/'), "{:?} is not a path", argv[0]);
        assert!(argv.len() <= MAX_ARGV + 1, "{argv:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_implausible_pid_yields_no_arguments_rather_than_failing() {
        assert!(process_argv(u32::MAX).is_empty());
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
