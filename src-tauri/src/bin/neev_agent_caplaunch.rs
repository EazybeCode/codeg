//! Fixed-command privileged launch helper for the sandboxed agent runtime.
//!
//! This is the ONLY privileged component of the agent runtime. It is installed
//! with file capabilities (`cap_chown,cap_dac_override,cap_setgid,cap_setuid,
//! cap_setpcap,cap_net_admin,cap_sys_admin=eip`) so the UNPRIVILEGED web server
//! (uid 65532, no effective caps) can exec it. It:
//!   1. moves those caps into its inheritable set and raises them to the AMBIENT
//!      set (so they survive the exec into the Python launcher and its
//!      `unshare`/`ip`/`nft` children), then
//!   2. exec's a FIXED command — `/usr/bin/python3 <fixed launcher>` — passing
//!      through only the launcher's arguments.
//!
//! It deliberately execs a hardcoded interpreter + launcher path (never anything
//! from argv[0]/PATH/env), so an attacker who can exec this binary can only run
//! the sandbox launcher (which validates its own args and drops to the tenant uid
//! with no caps) — NOT an arbitrary privileged command. Contrast a setcap'd
//! general tool (e.g. `setpriv`), which would be a trivial root-escalation
//! primitive for any RCE as the server uid.
//!
//! Linux only; on other targets it refuses (the caps/ambient model is Linux).

// Fixed, hardcoded targets — never taken from argv[0], PATH, or the environment.
const PYTHON: &str = "/usr/bin/python3";
const LAUNCHER: &str = "/opt/neev/scripts/operations/neev_agent_launch.py";
// The trusted workspace root, passed to the launcher in the SANITIZED env so the
// launcher can require --workdir to resolve strictly under it (a caller cannot
// override it, since we clear the environment below).
const WORKSPACE_ROOT: &str = "/data/agents";
const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

// Capability numbers (linux/capability.h) — defined locally so we don't depend on
// the libc crate exporting CAP_* constants. The exact caps the sandbox launcher
// needs (and no more): chown the per-run workdir, drop to the tenant uid/gid, drop
// the capability bounding set, create namespaces, wire the egress netns/nft, and
// mkdir under root-owned /run.
#[cfg(target_os = "linux")]
mod cap {
    pub const CHOWN: i32 = 0;
    pub const DAC_OVERRIDE: i32 = 1;
    pub const SETGID: i32 = 6;
    pub const SETUID: i32 = 7;
    pub const SETPCAP: i32 = 8;
    pub const NET_ADMIN: i32 = 12;
    pub const SYS_ADMIN: i32 = 21;
    // prctl(2) ambient-capability selectors.
    pub const PR_CAP_AMBIENT: i32 = 47;
    pub const PR_CAP_AMBIENT_RAISE: u64 = 2;
}

#[cfg(target_os = "linux")]
const CAPS: [i32; 7] = [
    cap::CHOWN,
    cap::DAC_OVERRIDE,
    cap::SETGID,
    cap::SETUID,
    cap::SETPCAP,
    cap::NET_ADMIN,
    cap::SYS_ADMIN,
];

#[cfg(target_os = "linux")]
fn raise_ambient() -> std::io::Result<()> {
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    // _LINUX_CAPABILITY_VERSION_3 (64-bit caps => two data words).
    let mut header = CapHeader { version: 0x2008_0522, pid: 0 };
    let mut data = [CapData { effective: 0, permitted: 0, inheritable: 0 }; 2];
    // SAFETY: capget fills `data` for the current thread; sizes match the v3 ABI.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_capget,
            &mut header as *mut CapHeader,
            data.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Add each cap (already in the file-granted permitted set) to inheritable —
    // PR_CAP_AMBIENT_RAISE requires the cap in permitted AND inheritable.
    for &c in CAPS.iter() {
        let idx = (c >> 5) as usize;
        data[idx].inheritable |= 1u32 << (c & 31);
    }
    let rc = unsafe {
        libc::syscall(
            libc::SYS_capset,
            &mut header as *mut CapHeader,
            data.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for &c in CAPS.iter() {
        // SAFETY: prctl with fixed selectors; c is a valid capability number.
        let rc = unsafe {
            libc::prctl(
                cap::PR_CAP_AMBIENT,
                cap::PR_CAP_AMBIENT_RAISE as libc::c_ulong,
                c as libc::c_ulong,
                0,
                0,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::os::unix::process::CommandExt;
    if let Err(error) = raise_ambient() {
        eprintln!("neev-agent-caplaunch: cannot raise capabilities: {error}");
        return std::process::ExitCode::from(3);
    }
    // Pass through ONLY the launcher's arguments; the command itself is fixed.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // CRITICAL: sanitize the environment before exec. We hold ambient caps here,
    // so python MUST NOT honor attacker-controlled PYTHONPATH/PYTHONSTARTUP/
    // sitecustomize or LD_PRELOAD/LD_LIBRARY_PATH (any of which would run arbitrary
    // code with these caps BEFORE the launcher/setpriv drop). `env_clear` drops all
    // inherited env; `-I` runs python in isolated mode (ignores PYTHON* + user
    // site); we set only a fixed PATH and the trusted workspace root.
    let error = std::process::Command::new(PYTHON)
        .arg0("python3")
        .arg("-I")
        .arg(LAUNCHER)
        .args(&args)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .env("NEEV_AGENT_WORKSPACE_ROOT", WORKSPACE_ROOT)
        .exec(); // replaces this process on success; ambient caps survive
    eprintln!("neev-agent-caplaunch: exec {PYTHON} {LAUNCHER} failed: {error}");
    std::process::ExitCode::from(4)
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("neev-agent-caplaunch: Linux only");
    std::process::ExitCode::from(2)
}
