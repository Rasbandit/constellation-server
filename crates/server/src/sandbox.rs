//! The one way the server runs the generation sidecar (`tools/ap-gen/ap_gen.py`): inside
//! bubblewrap, because generating runs the Python code of uploaded worlds.
//!
//! Every call gets fresh namespaces with no network (`--unshare-all`), no further user
//! namespaces (`--disable-userns`), no capabilities, a new session and an empty environment.
//! The whole tree is read-only (`/`, `/dev`, `/usr`, the Python install, the sidecar's own
//! folders under `/tools`) except size-capped tmpfs mounts (`/tmp`, and any scratch folder the
//! caller names). No host folder is ever writable: a result comes back on stdout only. With the
//! Archipelago checkout it is `/ap`, read-only, whose `worlds/` shows only its top-level `.py`
//! files and `generic` (the core worlds are neither needed nor trusted, and importing all of
//! them costs seconds), and whose `custom_worlds/` is the caller's mount. The Python process
//! runs under `prlimit` (address space, processes, file size), is killed at its wall-clock
//! limit, and dies with the server (`--die-with-parent`). Its stdout is capped and its stderr
//! is cut to a bounded tail while it is read, so a flood never builds up in the server.
//!
//! Known limits, accepted for now: the address space cap is per process (all of them together
//! can use `gen_max_processes` times it; run the server under a cgroup memory cap such as
//! systemd's `MemoryMax`), there is no seccomp filter (bubblewrap takes a compiled BPF
//! program, which needs libseccomp or a hand-written filter), and CPU is bounded only by the
//! time limit.

use config::{Generation, Limits};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// The checkout inside the sandbox.
pub const AP: &str = "/ap";
/// The job's own files inside the sandbox.
pub const JOB: &str = "/job";

/// What the caller adds to the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    /// A host folder shown read-only at a path inside.
    Ro(PathBuf, String),
    /// A size-capped tmpfs at a path inside, gone when the call ends.
    Scratch(String),
}

/// Why a call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The sidecar ran and refused or failed: about the input (its stderr tail and status).
    Refused(String),
    /// The sandbox itself failed: no bwrap, a timeout, unreadable or oversized output.
    Broken(String),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(s) | Self::Broken(s) => f.write_str(s),
        }
    }
}

/// Runs the sidecar in bubblewrap.
#[derive(Debug, Clone)]
pub struct Sandbox {
    paths: Generation,
    limits: Limits,
    python: PythonPlan,
    /// Host folders shown read-only under `/tools`: (host, inside).
    tools: Vec<(PathBuf, String)>,
    /// The checkout's top-level `worlds/*.py` files.
    world_files: Vec<PathBuf>,
}

/// How the Python install appears inside: folders mounted at their own path, and symlinks
/// recreated (target, link) for links that live outside those folders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PythonPlan {
    binds: Vec<PathBuf>,
    links: Vec<(PathBuf, PathBuf)>,
}

impl Sandbox {
    /// A sandbox for these paths. Refuses mounts that would show too much of the host: the
    /// root, or any folder holding `data_dir` (uploads, other jobs, maybe the token file).
    ///
    /// # Errors
    /// A path that is missing, not a usable layout, or too broad to mount.
    pub fn new(paths: Generation, limits: Limits, data_dir: &Path) -> Result<Self, String> {
        let python = python_plan(&paths.python)?;
        let side = &paths.sidecar;
        let folder = side.parent().ok_or("the sidecar has no folder")?;
        let name = folder.file_name().ok_or("the sidecar's folder has no name")?;
        let mut tools = vec![(folder.to_owned(), format!("/tools/{}", name.to_string_lossy()))];
        // ap_gen.py imports the bundle converter from its sibling folder.
        if let Some(import) = folder.parent().map(|t| t.join("ap-import")).filter(|p| p.is_dir()) {
            tools.push((import, "/tools/ap-import".to_owned()));
        }
        let ap = &paths.ap_dir;
        for need in ["custom_worlds", "worlds/generic", "worlds/__init__.py"] {
            if !ap.join(need).exists() {
                return Err(format!("{}: no {need}; not an Archipelago checkout", ap.display()));
            }
        }
        let mut world_files: Vec<PathBuf> = std::fs::read_dir(ap.join("worlds"))
            .map_err(|e| format!("{}: {e}", ap.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "py"))
            .collect();
        world_files.sort();
        let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        // Never shown to uploaded code: the data folder, the home folder, the secret files.
        let private: Vec<PathBuf> = [data_dir]
            .into_iter()
            .chain(paths.private.iter().map(PathBuf::as_path))
            .map(real)
            .collect();
        let sources = python.binds.iter().chain(tools.iter().map(|(h, _)| h)).chain([ap]);
        for src in sources {
            let src_real = real(src);
            if src_real == Path::new("/") || private.iter().any(|p| p.starts_with(&src_real)) {
                return Err(format!(
                    "{} would show the whole host, the home folder, the data folder or a \
                     secret file to uploaded code; move it into its own folder",
                    src.display()
                ));
            }
        }
        Ok(Self { paths, limits, python, tools, world_files })
    }

    /// The full `bwrap` command line for one call (program first).
    #[must_use]
    pub fn argv(&self, with_ap: bool, mounts: &[Mount], args: &[&str]) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        let mut push = |xs: &[&dyn AsRef<std::ffi::OsStr>]| {
            a.extend(xs.iter().map(|x| x.as_ref().to_owned()));
        };
        // Its own cgroup first: the memory cap there also counts tmpfs, memfd and shared memory
        // pages, and an out-of-memory kill lands on the sandbox, not the server. `env -i`:
        // bwrap's own process stays in the sandbox as its first process, and its environment
        // is readable there (`/proc/1/environ`), so it gets none of ours (the admin token).
        let l = &self.limits;
        let (mem_max, tasks) = (
            format!("MemoryMax={}", l.gen_cgroup_memory_bytes),
            format!("TasksMax={}", l.gen_cgroup_tasks),
        );
        if self.paths.container_mode {
            // The container's own memory and pids limits bound the job instead (compose
            // `mem_limit`, `pids_limit`); every bwrap isolation flag below stays.
            push(&[&"env", &"-i"]);
        } else {
            push(&[&"systemd-run", &"--user", &"--scope", &"--quiet", &"--collect"]);
            push(&[
                &"-p",
                &mem_max,
                &"-p",
                &"MemorySwapMax=0",
                &"-p",
                &tasks,
                &"--",
                &"env",
                &"-i",
            ]);
        }
        push(&[&"bwrap", &"--unshare-all", &"--unshare-user", &"--disable-userns"]);
        push(&[&"--cap-drop", &"ALL", &"--die-with-parent", &"--new-session", &"--clearenv"]);
        push(&[&"--setenv", &"PATH", &"/usr/bin", &"--setenv", &"HOME", &"/tmp"]);
        push(&[&"--setenv", &"PYTHONDONTWRITEBYTECODE", &"1"]);
        push(&[&"--ro-bind", &"/usr", &"/usr", &"--symlink", &"usr/lib64", &"/lib64"]);
        push(&[&"--symlink", &"usr/lib", &"/lib", &"--symlink", &"usr/bin", &"/bin"]);
        push(&[&"--proc", &"/proc", &"--dev", &"/dev"]);
        let size = self.limits.gen_tmpfs_bytes.to_string();
        push(&[&"--size", &size, &"--tmpfs", &"/tmp"]);
        for root in &self.python.binds {
            push(&[&"--ro-bind", root, root]);
        }
        for (target, link) in &self.python.links {
            push(&[&"--symlink", target, link]);
        }
        for (host, inside) in &self.tools {
            push(&[&"--ro-bind", host, inside]);
        }
        if with_ap {
            let ap = &self.paths.ap_dir;
            push(&[&"--ro-bind", ap, &AP, &"--size", &size, &"--tmpfs", &"/ap/worlds"]);
            for f in &self.world_files {
                let name = f.file_name().unwrap_or_default().to_string_lossy();
                push(&[&"--ro-bind", f, &format!("{AP}/worlds/{name}")]);
            }
            push(&[&"--ro-bind", &ap.join("worlds/generic"), &"/ap/worlds/generic"]);
            push(&[&"--remount-ro", &"/ap/worlds"]);
        }
        for m in mounts {
            match m {
                Mount::Ro(host, inside) => push(&[&"--ro-bind", host, inside]),
                Mount::Scratch(inside) => push(&[&"--size", &size, &"--tmpfs", inside]),
            }
        }
        push(&[&"--remount-ro", &"/dev", &"--remount-ro", &"/"]);
        let script = format!("{}/{}", self.tools.first().map_or("", |(_, i)| i), {
            self.paths.sidecar.file_name().unwrap_or_default().to_string_lossy()
        });
        let l = &self.limits;
        let (mem, procs) =
            (format!("--as={}", l.gen_memory_bytes), format!("--nproc={}", l.gen_max_processes));
        let fsize = format!("--fsize={}", l.gen_file_max_bytes);
        let nofile = format!("--nofile={}", l.gen_max_open_files);
        push(&[&"--chdir", &if with_ap { AP } else { "/tmp" }, &"/usr/bin/prlimit"]);
        push(&[&mem, &procs, &fsize, &nofile, &"--"]);
        // -I: ignore PYTHON* variables, the user site and the script's folder on sys.path.
        push(&[&self.paths.python, &"-I", &script]);
        a.extend(args.iter().map(OsString::from));
        a
    }

    /// Run the sidecar and return its stdout (at most `Limits::gen_stdout_max_bytes`).
    ///
    /// # Errors
    /// [`Failure::Refused`] with the end of its error output when the sidecar fails;
    /// [`Failure::Broken`] on a spawn failure, a timeout or bad or oversized output.
    pub async fn run(
        &self,
        with_ap: bool,
        mounts: &[Mount],
        args: &[&str],
        timeout: Duration,
    ) -> Result<Vec<u8>, Failure> {
        exec(&self.argv(with_ap, mounts, args), &[], &self.limits, timeout, false).await
    }
}

/// The command line of a data-only job: this very executable in `job` mode (`fill-job`, see
/// [`fill_job`]), under `prlimit` caps on address space, CPU time, open files and file size. No
/// sandbox: the job reads data, never code; the caps and the wall-clock kill bound what a huge
/// or hostile manifest can cost.
#[must_use]
pub fn job_argv(exe: &Path, job: &str, l: &Limits) -> Vec<OsString> {
    let caps = [
        format!("--as={}", l.fill_memory_bytes),
        format!("--cpu={}", l.fill_cpu_secs),
        format!("--nofile={}", l.gen_max_open_files),
        "--fsize=0".to_owned(),
    ]
    .into_iter()
    .chain((l.fill_max_processes > 0).then(|| format!("--nproc={}", l.fill_max_processes)));
    let mut a: Vec<OsString> = vec!["/usr/bin/prlimit".into()];
    a.extend(caps.into_iter().map(OsString::from));
    a.extend(["--".into(), exe.into(), "fill-job".into(), job.into()]);
    a
}

/// Run a data-only job (`generate` or `inspect`) in a capped child process: `input` on its
/// stdin, its stdout back. A child past `timeout` is killed and reaped before this returns.
///
/// # Errors
/// As [`Sandbox::run`].
pub async fn run_job(
    exe: &Path,
    job: &str,
    input: &[u8],
    l: &Limits,
    timeout: Duration,
) -> Result<Vec<u8>, Failure> {
    exec(&job_argv(exe, job, l), input, l, timeout, true).await
}

/// This executable's path, for [`run_job`]. After the file is replaced under a running process
/// (an in-place deploy) Linux reports it as `<path> (deleted)`; the path without that suffix is
/// where the replacement is.
///
/// # Errors
/// When the operating system cannot say.
pub fn exe_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    Ok(without_deleted(exe))
}

fn without_deleted(exe: PathBuf) -> PathBuf {
    let shown = exe.to_string_lossy();
    match shown.strip_suffix(" (deleted)") {
        Some(path) if !exe.exists() => PathBuf::from(path),
        _ => exe,
    }
}

/// The child side of [`run_job`]: read stdin, run the job, print the result.
///
/// # Errors
/// What the job refused, or an IO error on the pipes.
pub fn fill_job(job: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).map_err(|e| e.to_string())?;
    let out = match job {
        "generate" => data_fill::job::generate(&input)?,
        "inspect" => data_fill::job::inspect(&input)?,
        other => return Err(format!("unknown job {other:?}")),
    };
    std::io::stdout().write_all(&out).map_err(|e| e.to_string())
}

/// Run `argv` (program first) with `input` on its stdin, and return its stdout (at most
/// `Limits::gen_stdout_max_bytes`); see [`Sandbox::run`]. With `clean` the child gets no
/// environment at all: the server's token and database settings are in its own.
async fn exec(
    argv: &[OsString],
    input: &[u8],
    l: &Limits,
    timeout: Duration,
    clean: bool,
) -> Result<Vec<u8>, Failure> {
    let (program, rest) = argv.split_first().ok_or(Failure::Broken("empty command".into()))?;
    let mut command = tokio::process::Command::new(program);
    if clean {
        command.env_clear();
    }
    let mut child = command
        .args(rest)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Dropping the child on timeout kills bwrap; `--die-with-parent` takes the rest.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Failure::Broken(format!("cannot start the sandbox (is bwrap installed?): {e}")))?;
    let mut stdin = child.stdin.take();
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(Failure::Broken("no pipes to the sandbox".into()));
    };
    let feed = async move {
        // A child that stops reading closes the pipe: its exit status says why.
        if let Some(mut pipe) = stdin.take() {
            drop(tokio::io::AsyncWriteExt::write_all(&mut pipe, input).await);
        }
    };
    let all = async {
        tokio::join!(
            feed,
            drain(out, l.gen_stdout_max_bytes, Keep::Head),
            drain(err, l.gen_error_max_bytes, Keep::Tail),
            child.wait()
        )
    };
    let Ok(((), out, err, status)) = tokio::time::timeout(timeout, all).await else {
        // Kill bwrap and reap it before reporting; its sandbox goes with it.
        if let Err(e) = child.kill().await {
            tracing::warn!(error = %e, "could not kill a timed-out sandbox");
        }
        return Err(Failure::Broken(format!(
            "timed out after {} s and was killed",
            timeout.as_secs()
        )));
    };
    let broken = |e: std::io::Error| Failure::Broken(format!("sandbox: {e}"));
    let (status, (out, out_total), (err, _)) =
        (status.map_err(broken)?, out.map_err(broken)?, err.map_err(broken)?);
    if !status.success() {
        let err = String::from_utf8_lossy(&err);
        return Err(Failure::Refused(format!("{} ({status})", err.trim())));
    }
    if out_total > l.gen_stdout_max_bytes as u64 {
        return Err(Failure::Broken(format!(
            "the sidecar printed more than {} bytes",
            l.gen_stdout_max_bytes
        )));
    }
    Ok(out)
}

/// Check once at startup that each job can get its own cgroup (`systemd-run --user`
/// needs a user session with the memory controller); without it no job could run. In
/// container mode there is no such cgroup, so nothing is checked and a warning says so.
///
/// # Errors
/// What `systemd-run` said.
pub fn check_host(container_mode: bool, l: &Limits) -> Result<(), String> {
    if container_mode {
        tracing::warn!(
            "sandbox mode CONTAINER: no per-job cgroup; uploaded code is bounded only by \
             prlimit and the container's own memory and pids limits (set mem_limit and pids_limit)"
        );
        return Ok(());
    }
    let out = std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect", "-p"])
        .arg(format!("MemoryMax={}", l.gen_cgroup_memory_bytes))
        .args(["--", "true"])
        .output()
        .map_err(|e| format!("cannot run systemd-run for the sandbox's cgroup: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "the sandbox needs `systemd-run --user --scope` (a user session with the memory \
             controller): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Which bytes of a stream [`drain`] keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    Head,
    Tail,
}

/// Read `r` to its end, keeping at most `max` bytes (its start or its end), and return them
/// with the total length. Never holds more than about twice `max` plus one read.
async fn drain<R: AsyncRead + Unpin>(
    mut r: R,
    max: usize,
    keep: Keep,
) -> std::io::Result<(Vec<u8>, u64)> {
    let (mut kept, mut total, mut buf) = (Vec::new(), 0_u64, vec![0_u8; 64 * 1024]);
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        total += n as u64;
        let chunk = buf.get(..n).unwrap_or_default();
        match keep {
            Keep::Head => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(chunk.get(..room.min(n)).unwrap_or_default());
            }
            Keep::Tail => push_tail(&mut kept, chunk, max),
        }
    }
    // A tail can be up to twice `max` here; a head never passes `max`.
    kept.drain(..kept.len().saturating_sub(max));
    Ok((kept, total))
}

/// Add `chunk` to the end of `kept` and cut it back to its last `max` bytes once it holds
/// more than twice that: one cut per `max` bytes read, not one per read.
fn push_tail(kept: &mut Vec<u8>, chunk: &[u8], max: usize) {
    kept.extend_from_slice(chunk);
    if kept.len() > 2 * max {
        kept.drain(..kept.len() - max);
    }
}

/// How to show the Python at `python` inside: the venv (the folder with `pyvenv.cfg`) and the
/// real install it runs, each at its own path; another name of that install (a symlinked
/// folder) mounted too; and any other symlink on the way recreated as a symlink, so a link
/// in a broad folder (`~/.local/bin`) never mounts that folder.
fn python_plan(python: &Path) -> Result<PythonPlan, String> {
    let real = std::fs::canonicalize(python).map_err(|e| format!("{}: {e}", python.display()))?;
    let install = real.parent().and_then(Path::parent).ok_or("python is not in <prefix>/bin")?;
    let mut plan = PythonPlan::default();
    let usr = |p: &Path| p.starts_with("/usr");
    if !usr(install) {
        plan.binds.push(install.to_owned());
    }
    if let Some(venv) =
        python.parent().and_then(Path::parent).filter(|v| v.join("pyvenv.cfg").is_file())
    {
        plan.binds.push(venv.to_owned());
    }
    let mut p = python.to_owned();
    for _ in 0..16 {
        let under = |p: &Path| usr(p) || plan.binds.iter().any(|b| p.starts_with(b));
        let link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
        if !under(&p) {
            let alias = p.parent().and_then(Path::parent).map(Path::to_owned);
            match alias {
                Some(a) if std::fs::canonicalize(&a).is_ok_and(|c| c == install) => {
                    plan.binds.push(a);
                }
                _ if link => {
                    let target = std::fs::canonicalize(&p).map_err(|e| e.to_string())?;
                    plan.links.push((target, p.clone()));
                }
                _ => {
                    return Err(format!(
                        "{}: cannot place this python in the sandbox",
                        p.display()
                    ));
                }
            }
        }
        if !link {
            break;
        }
        let target = std::fs::read_link(&p).map_err(|e| e.to_string())?;
        p = p.parent().map_or_else(|| target.clone(), |dir| dir.join(&target));
    }
    Ok(plan)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sandbox-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&d));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake checkout and tools folder under `root`.
    pub(crate) fn layout(root: &Path) -> Generation {
        for d in [
            "ap/custom_worlds",
            "ap/worlds/generic",
            "ap/worlds/alttp",
            "tools/ap-gen",
            "tools/ap-import",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["ap/worlds/__init__.py", "ap/worlds/AutoWorld.py", "ap/worlds/notes.txt"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        Generation {
            ap_dir: root.join("ap"),
            python: "/usr/bin/python3".into(),
            sidecar: root.join("tools/ap-gen/ap_gen.py"),
            private: vec![],
            container_mode: false,
        }
    }

    fn sandbox(root: &Path) -> Sandbox {
        let limits = Limits { gen_memory_bytes: 1000, gen_max_processes: 7, ..Limits::DESIGN };
        Sandbox::new(layout(root), limits, &root.join("data")).unwrap()
    }

    fn joined(v: &[OsString]) -> String {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    /// Container mode drops only the per-job cgroup: every bwrap flag and prlimit stay.
    #[test]
    fn container_mode_has_no_systemd_run_but_every_isolation_flag() {
        let root = temp("argv-container");
        let normal = sandbox(&root);
        let container = Sandbox {
            paths: Generation { container_mode: true, ..normal.paths.clone() },
            ..sandbox(&root)
        };
        let args = ["inspect-yaml", "/job/x.yaml"];
        let (n, c) =
            (joined(&normal.argv(false, &[], &args)), joined(&container.argv(false, &[], &args)));
        assert!(!c.contains("systemd-run") && !c.contains("MemoryMax"), "{c}");
        let rest = n.split_once(" -- env -i ").unwrap().1;
        assert_eq!(c, format!("env -i {rest}"), "everything after the cgroup is identical");
        for flag in [
            "--unshare-all",
            "--unshare-user",
            "--disable-userns",
            "--cap-drop ALL",
            "--die-with-parent",
            "--clearenv",
            "/usr/bin/prlimit",
        ] {
            assert!(c.contains(flag), "{flag}: {c}");
        }
    }

    #[test]
    fn every_call_is_isolated_read_only_and_capped() {
        let root = temp("argv");
        let a = joined(&sandbox(&root).argv(false, &[], &["inspect-yaml", "/job/x.yaml"]));
        assert!(a.starts_with("systemd-run --user --scope --quiet --collect -p MemoryMax=4294967296 -p MemorySwapMax=0 -p TasksMax=80 -- env -i bwrap --unshare-all --unshare-user --disable-userns --cap-drop ALL --die-with-parent --new-session --clearenv"), "{a}");
        assert!(
            !a.contains("--share-net") && !a.contains("--bind ") && !a.contains("overlay"),
            "{a}"
        );
        assert!(
            a.contains(&format!("--size {} --tmpfs /tmp", Limits::DESIGN.gen_tmpfs_bytes)),
            "{a}"
        );
        assert!(
            a.contains("--remount-ro /dev --remount-ro / --chdir /tmp /usr/bin/prlimit"),
            "{a}"
        );
        assert!(a.contains("--as=1000 --nproc=7 --fsize=268435456 --nofile=256 -- /usr/bin/python3 -I /tools/ap-gen/ap_gen.py inspect-yaml /job/x.yaml"), "{a}");
        let tools = root.join("tools");
        assert!(a.contains(&format!("--ro-bind {}/ap-gen /tools/ap-gen", tools.display())), "{a}");
        assert!(
            a.contains(&format!("--ro-bind {}/ap-import /tools/ap-import", tools.display())),
            "{a}"
        );
        assert!(
            !a.contains(&format!("{} /tools ", tools.display())),
            "never the whole tools folder: {a}"
        );
        assert!(!a.contains("/ap/"), "inspect never sees the checkout: {a}");
    }

    #[test]
    fn generation_sees_only_generic_the_chosen_worlds_and_a_scratch_folder() {
        let root = temp("gen");
        let mounts = [
            Mount::Ro(root.join("data/worlds/1/apworlds"), "/ap/custom_worlds".into()),
            Mount::Scratch("/job/out".into()),
        ];
        let a = joined(&sandbox(&root).argv(true, &mounts, &["generate"]));
        let ap = root.join("ap");
        assert!(a.contains(&format!("--ro-bind {} /ap --size", ap.display())), "{a}");
        assert!(a.contains(&format!("--ro-bind {0}/worlds/AutoWorld.py /ap/worlds/AutoWorld.py --ro-bind {0}/worlds/__init__.py /ap/worlds/__init__.py --ro-bind {0}/worlds/generic /ap/worlds/generic --remount-ro /ap/worlds", ap.display())), "{a}");
        assert!(!a.contains("alttp") && !a.contains("notes.txt"), "{a}");
        assert!(a.contains("/apworlds /ap/custom_worlds"), "{a}");
        let scratch = a.find("--tmpfs /job/out").unwrap();
        assert!(scratch < a.find("--remount-ro / ").unwrap(), "{a}");
    }

    #[test]
    fn mounts_that_hold_the_data_folder_or_the_root_are_refused() {
        let root = temp("broad");
        let g = layout(&root);
        assert!(Sandbox::new(g.clone(), Limits::DESIGN, &root.join("data")).is_ok());
        let e = Sandbox::new(g.clone(), Limits::DESIGN, &root.join("ap/data")).unwrap_err();
        assert!(e.contains("data folder"), "{e}");
        let e = Sandbox::new(g.clone(), Limits::DESIGN, &root.join("tools/ap-gen/d")).unwrap_err();
        assert!(e.contains("data folder"), "{e}");
        let at_root = Generation { sidecar: "/ap_gen.py".into(), ..g.clone() };
        assert!(Sandbox::new(at_root, Limits::DESIGN, &root.join("data")).is_err());
        // The home folder itself, or a folder holding a secret file.
        let home = Generation { private: vec![root.join("tools/ap-gen")], ..g.clone() };
        let e = Sandbox::new(home, Limits::DESIGN, &root.join("data")).unwrap_err();
        assert!(e.contains("home folder"), "{e}");
        let token = Generation { private: vec![root.join("ap/token")], ..g.clone() };
        assert!(Sandbox::new(token, Limits::DESIGN, &root.join("data")).is_err());
        let home_above = Generation { private: vec![root.clone()], ..g.clone() };
        assert!(
            Sandbox::new(home_above, Limits::DESIGN, &root.join("data")).is_ok(),
            "below home is fine"
        );
        let not_ap = Generation { ap_dir: root.join("tools"), ..g };
        assert!(
            Sandbox::new(not_ap, Limits::DESIGN, &root.join("data"))
                .unwrap_err()
                .contains("checkout")
        );
    }

    #[tokio::test]
    async fn a_flood_is_read_to_the_end_but_only_a_bounded_part_is_kept() {
        let flood = || tokio::io::repeat(b'x').take(10 * 1024 * 1024);
        let (head, total) = drain(flood(), 1000, Keep::Head).await.unwrap();
        assert_eq!((head.len(), total), (1000, 10 * 1024 * 1024));
        let (tail, total) = drain(flood(), 1000, Keep::Tail).await.unwrap();
        assert_eq!((tail.len(), total), (1000, 10 * 1024 * 1024));
        let (tail, _) = drain(&b"abcdef"[..], 3, Keep::Tail).await.unwrap();
        assert_eq!(tail, b"def");
        let (all, total) = drain(&b"ab"[..], 3, Keep::Head).await.unwrap();
        assert_eq!((all.as_slice(), total), (&b"ab"[..], 2));
        let (none, total) = drain(&b""[..], 3, Keep::Tail).await.unwrap();
        assert_eq!((none.len(), total), (0, 0));
    }

    #[test]
    fn a_tail_is_cut_back_to_its_cap_each_time_it_passes_twice_that() {
        let (mut kept, mut lens) = (Vec::new(), Vec::new());
        for i in 0..8_u8 {
            push_tail(&mut kept, &[i; 5], 10);
            lens.push(kept.len());
        }
        assert_eq!(lens, [5, 10, 15, 20, 10, 15, 20, 10]);
        assert_eq!(kept, [[6_u8; 5], [7; 5]].concat());
    }

    fn sh(script: &str) -> Vec<OsString> {
        ["/bin/sh", "-c", script].map(OsString::from).to_vec()
    }

    /// The bound on these calls is a guard only: each command ends at once.
    const NEVER: Duration = Duration::from_secs(60);

    #[tokio::test]
    async fn a_result_of_exactly_the_stdout_cap_is_kept_and_one_byte_more_is_refused() {
        let l = Limits { gen_stdout_max_bytes: 5, ..Limits::DESIGN };
        assert_eq!(exec(&sh("printf 12345"), &[], &l, NEVER, false).await.unwrap(), b"12345");
        let e = exec(&sh("printf 123456"), &[], &l, NEVER, false).await.unwrap_err();
        assert_eq!(e, Failure::Broken("the sidecar printed more than 5 bytes".into()));
    }

    #[tokio::test]
    async fn a_sidecar_that_fails_is_refused_with_the_end_of_its_error_output() {
        let l = Limits { gen_error_max_bytes: 4, ..Limits::DESIGN };
        let fails = sh("printf out; printf 'long reason' >&2; exit 3");
        let e = exec(&fails, &[], &l, NEVER, false).await.unwrap_err();
        assert_eq!(e, Failure::Refused("ason (exit status: 3)".into()));
    }

    #[tokio::test]
    async fn input_reaches_the_child_and_a_child_past_its_time_is_killed_not_left_running() {
        let l = Limits::DESIGN;
        assert_eq!(exec(&sh("cat"), b"fed in", &l, NEVER, false).await.unwrap(), b"fed in");
        let root = temp("kill");
        let pidfile = root.join("pid");
        let hang = sh(&format!("echo $$ > {}; exec sleep 30", pidfile.display()));
        let e = exec(&hang, &[], &l, Duration::from_millis(300), false).await.unwrap_err();
        assert!(matches!(&e, Failure::Broken(m) if m.contains("killed")), "{e:?}");
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "the child {pid} is gone");
    }

    /// The server's own environment holds its token and database settings; a job child that
    /// parses hostile bytes must get none of it.
    #[tokio::test]
    async fn a_clean_child_sees_no_environment_and_an_unclean_one_does() {
        let l = Limits::DESIGN;
        let env = ["/usr/bin/env"].map(OsString::from).to_vec();
        assert_eq!(exec(&env, &[], &l, NEVER, true).await.unwrap(), b"");
        let all = exec(&env, &[], &l, NEVER, false).await.unwrap();
        assert!(String::from_utf8_lossy(&all).contains("PATH="), "the test process has a PATH");
    }

    #[test]
    fn a_replaced_binary_is_still_found_at_its_path() {
        let root = temp("deleted-exe");
        let live = root.join("server");
        std::fs::write(&live, "").unwrap();
        let shown = PathBuf::from(format!("{} (deleted)", live.display()));
        assert_eq!(without_deleted(shown.clone()), live, "a replacement sits at the old path");
        std::fs::remove_file(&live).unwrap();
        assert_eq!(without_deleted(shown.clone()), live, "the suffix goes either way");
        assert_eq!(without_deleted(live.clone()), live, "a plain path is left as it is");
        // A file really named with that suffix is itself, not a replaced binary.
        std::fs::write(&shown, "").unwrap();
        assert_eq!(without_deleted(shown.clone()), shown);
        assert!(exe_path().unwrap().exists());
    }

    #[test]
    fn a_job_child_runs_this_executable_under_the_fill_caps() {
        let l = Limits {
            fill_memory_bytes: 7,
            fill_cpu_secs: 3,
            fill_max_processes: 9,
            ..Limits::DESIGN
        };
        let a: Vec<String> = job_argv(Path::new("/x/server"), "generate", &l)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(a[0], "/usr/bin/prlimit");
        assert!(a.contains(&"--as=7".to_owned()) && a.contains(&"--cpu=3".to_owned()), "{a:?}");
        assert!(a.contains(&"--nproc=9".to_owned()), "{a:?}");
        let off = Limits { fill_max_processes: 0, ..l };
        let a = job_argv(Path::new("/x/server"), "generate", &off);
        assert!(!a.iter().any(|x| x.to_string_lossy().starts_with("--nproc")));
        assert_eq!(a[a.len() - 3..], ["/x/server", "fill-job", "generate"]);
    }

    #[test]
    fn a_failure_shows_its_reason_alone() {
        assert_eq!(Failure::Refused("bad yaml".into()).to_string(), "bad yaml");
        assert_eq!(Failure::Broken("timed out".into()).to_string(), "timed out");
    }

    /// A cgroup too small to start any process: the check fails, as it does on a host with no
    /// user systemd session (there at connecting), so such a host is refused at startup.
    #[test]
    fn a_host_where_no_job_could_start_is_refused() {
        let tiny = Limits { gen_cgroup_memory_bytes: 1, ..Limits::DESIGN };
        let e = check_host(false, &tiny).unwrap_err();
        assert!(e.contains("systemd-run"), "{e}");
    }

    /// Only a symlinked file is recreated as a link; a python reached through a symlinked
    /// folder that is not its install is refused.
    #[test]
    fn a_python_behind_a_symlinked_bin_folder_is_refused() {
        let root = temp("py-dir-link");
        std::fs::create_dir_all(root.join("base/bin")).unwrap();
        std::fs::write(root.join("base/bin/python3"), "").unwrap();
        std::os::unix::fs::symlink(root.join("base/bin"), root.join("bin")).unwrap();
        let e = python_plan(&root.join("bin/python3")).unwrap_err();
        assert!(e.contains("cannot place"), "{e}");
    }

    #[test]
    fn a_venv_and_its_base_are_mounted_and_a_link_in_a_broad_folder_is_recreated() {
        let root = temp("py");
        let (base, venv, local) =
            (root.join("base"), root.join("venv"), root.join("home/.local/bin"));
        for d in [base.join("bin"), venv.join("bin"), local.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(base.join("bin/python3.12"), "").unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = x\n").unwrap();
        // venv/bin/python -> ~/.local/bin/python3.12 -> base/bin/python3.12, as uv lays it out.
        std::os::unix::fs::symlink(base.join("bin/python3.12"), local.join("python3.12")).unwrap();
        std::os::unix::fs::symlink(local.join("python3.12"), venv.join("bin/python")).unwrap();
        // A second name for the base install (a symlinked folder).
        std::os::unix::fs::symlink(&base, root.join("base-alias")).unwrap();
        let plan = python_plan(&venv.join("bin/python")).unwrap();
        assert_eq!(plan.binds, vec![base.clone(), venv.clone()]);
        assert_eq!(plan.links, vec![(base.join("bin/python3.12"), local.join("python3.12"))]);
        assert!(!plan.binds.iter().any(|b| b.starts_with(root.join("home"))));
        let alias = python_plan(&root.join("base-alias/bin/python3.12")).unwrap();
        assert_eq!(alias.binds, vec![base, root.join("base-alias")]);
        assert_eq!(python_plan(Path::new("/usr/bin/python3")).unwrap(), PythonPlan::default());
        assert!(python_plan(&root.join("missing")).is_err());
    }
}
