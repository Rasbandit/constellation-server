//! The one way the server runs the generation sidecar (`tools/ap-gen/ap_gen.py`): inside
//! bubblewrap, because generating runs the Python code of uploaded worlds.
//!
//! Every call gets: no network and fresh namespaces (`--unshare-all`), a new session, an empty
//! environment, `/usr` and the Python install read-only, a size-capped tmpfs at `/tmp`, the
//! sidecar's tools folder read-only at `/tools`, and only the mounts the caller names. With the
//! Archipelago checkout it is a throwaway overlay at `/ap` (Archipelago writes logs and
//! settings into its own folder, `Utils.user_path`; the writes vanish with the sandbox). The
//! Python process runs under `prlimit --as` (`Limits::gen_memory_bytes`), is killed at its
//! wall-clock limit, and dies with the server (`--die-with-parent`).

use config::{Generation, Limits};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The checkout inside the sandbox.
pub const AP: &str = "/ap";
/// The job's own files inside the sandbox.
pub const JOB: &str = "/job";

/// A host folder shown inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Folder on the host.
    pub host: PathBuf,
    /// Where it appears inside.
    pub inside: String,
    /// Read-write when true, else read-only.
    pub writable: bool,
}

impl Mount {
    /// A read-only mount.
    #[must_use]
    pub fn ro(host: &Path, inside: &str) -> Self {
        Self { host: host.to_owned(), inside: inside.to_owned(), writable: false }
    }

    /// A read-write mount.
    #[must_use]
    pub fn rw(host: &Path, inside: &str) -> Self {
        Self { host: host.to_owned(), inside: inside.to_owned(), writable: true }
    }
}

/// Runs the sidecar in bubblewrap.
#[derive(Debug, Clone)]
pub struct Sandbox {
    paths: Generation,
    limits: Limits,
}

/// Size of the sandbox's `/tmp`, bytes (design value: Archipelago only needs scratch space).
const TMP_BYTES: u64 = 256 * 1024 * 1024;

impl Sandbox {
    /// A sandbox for these paths.
    #[must_use]
    pub const fn new(paths: Generation, limits: Limits) -> Self {
        Self { paths, limits }
    }

    /// The full `bwrap` command line for one call (program first).
    ///
    /// # Errors
    /// When the sidecar path has no tools folder above it.
    pub fn argv(
        &self,
        with_ap: bool,
        mounts: &[Mount],
        args: &[&str],
    ) -> Result<Vec<OsString>, String> {
        let side = &self.paths.sidecar;
        let (Some(folder), Some(file)) =
            (side.parent().and_then(Path::file_name), side.file_name())
        else {
            return Err(format!("{}: want <tools>/<folder>/<script>", side.display()));
        };
        let tools = side.parent().and_then(Path::parent).ok_or("sidecar has no tools folder")?;
        let mut a: Vec<OsString> = Vec::new();
        let mut push = |xs: &[&dyn AsRef<std::ffi::OsStr>]| {
            a.extend(xs.iter().map(|x| x.as_ref().to_owned()));
        };
        push(&[&"bwrap", &"--unshare-all", &"--die-with-parent", &"--new-session", &"--clearenv"]);
        push(&[&"--setenv", &"PATH", &"/usr/bin", &"--setenv", &"HOME", &"/tmp"]);
        push(&[&"--setenv", &"PYTHONDONTWRITEBYTECODE", &"1"]);
        push(&[&"--ro-bind", &"/usr", &"/usr", &"--symlink", &"usr/lib64", &"/lib64"]);
        push(&[&"--symlink", &"usr/lib", &"/lib", &"--symlink", &"usr/bin", &"/bin"]);
        push(&[&"--proc", &"/proc", &"--dev", &"/dev"]);
        let tmp = TMP_BYTES.to_string();
        push(&[&"--size", &tmp, &"--tmpfs", &"/tmp"]);
        for root in python_roots(&self.paths.python) {
            push(&[&"--ro-bind", &root, &root]);
        }
        push(&[&"--ro-bind", &tools, &"/tools"]);
        if with_ap {
            push(&[&"--overlay-src", &self.paths.ap_dir, &"--tmp-overlay", &AP]);
        }
        for m in mounts {
            let how = if m.writable { "--bind" } else { "--ro-bind" };
            push(&[&how, &m.host, &m.inside]);
        }
        let script = Path::new("/tools").join(folder).join(file);
        let mem = format!("--as={}", self.limits.gen_memory_bytes);
        push(&[&"--chdir", &if with_ap { AP } else { "/tmp" }, &"/usr/bin/prlimit", &mem, &"--"]);
        // -I: ignore PYTHON* variables, the user site and the script's folder on sys.path.
        push(&[&self.paths.python, &"-I", &script]);
        a.extend(args.iter().map(OsString::from));
        Ok(a)
    }

    /// Run the sidecar and parse its stdout as JSON.
    ///
    /// # Errors
    /// The sidecar's error output (at most `Limits::gen_error_max_bytes`, its end), a timeout,
    /// or a spawn failure.
    pub async fn run(
        &self,
        with_ap: bool,
        mounts: &[Mount],
        args: &[&str],
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let argv = self.argv(with_ap, mounts, args)?;
        let (program, rest) = argv.split_first().ok_or("empty command")?;
        let child = tokio::process::Command::new(program)
            .args(rest)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Dropping the wait on timeout kills bwrap; `--die-with-parent` takes the rest.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start the sandbox (is bwrap installed?): {e}"))?;
        let out = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| format!("timed out after {} s and was killed", timeout.as_secs()))?
            .map_err(|e| format!("sandbox: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "{} ({})",
                tail(err.trim(), self.limits.gen_error_max_bytes),
                out.status
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("sidecar printed bad JSON: {e}"))
    }
}

/// The last `max` bytes of `s`, cut at a character boundary.
fn tail(s: &str, max: usize) -> &str {
    let mut from = s.len().saturating_sub(max);
    while !s.is_char_boundary(from) {
        from += 1;
    }
    s.get(from..).unwrap_or("")
}

/// The install folders (`<root>/bin/python`) a Python needs, following each symlink hop (a
/// venv links to its base install, which may itself be a link). Folders under `/usr` are
/// already mounted.
fn python_roots(python: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut add = |p: &Path| {
        if let Some(root) = p.parent().and_then(Path::parent)
            && !root.starts_with("/usr")
            && root != Path::new("/")
            && !roots.contains(&root.to_owned())
        {
            roots.push(root.to_owned());
        }
    };
    let mut p = python.to_owned();
    for _ in 0..16 {
        add(&p);
        let Ok(target) = std::fs::read_link(&p) else { break };
        p = p.parent().map_or_else(|| target.clone(), |dir| dir.join(&target));
    }
    if let Ok(real) = std::fs::canonicalize(python) {
        add(&real);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> Sandbox {
        Sandbox::new(
            Generation {
                ap_dir: "/srv/ap".into(),
                python: "/usr/bin/python3".into(),
                sidecar: "/srv/tools/ap-gen/ap_gen.py".into(),
            },
            Limits { gen_memory_bytes: 1000, ..Limits::DESIGN },
        )
    }

    fn joined(v: &[OsString]) -> String {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn every_call_has_no_network_dies_with_the_server_and_is_memory_capped() {
        let a = joined(&sandbox().argv(false, &[], &["inspect-yaml", "/job/x.yaml"]).unwrap());
        assert!(
            a.starts_with("bwrap --unshare-all --die-with-parent --new-session --clearenv"),
            "{a}"
        );
        assert!(!a.contains("--share-net"), "{a}");
        assert!(a.contains("/usr/bin/prlimit --as=1000 -- /usr/bin/python3 -I /tools/ap-gen/ap_gen.py inspect-yaml /job/x.yaml"), "{a}");
        assert!(a.contains("--ro-bind /srv/tools /tools"), "{a}");
        assert!(!a.contains("/srv/ap"), "inspect never sees the checkout: {a}");
        assert!(!a.contains("--bind "), "nothing is writable without a writable mount: {a}");
    }

    #[test]
    fn generation_sees_a_throwaway_checkout_and_only_its_own_writable_folder() {
        let mounts = [
            Mount::ro(Path::new("/data/worlds/1/apworlds"), "/ap/custom_worlds"),
            Mount::rw(Path::new("/data/worlds/1/out"), "/job/out"),
        ];
        let a = joined(&sandbox().argv(true, &mounts, &["generate"]).unwrap());
        let overlay = a.find("--overlay-src /srv/ap --tmp-overlay /ap").unwrap();
        let custom = a.find("--ro-bind /data/worlds/1/apworlds /ap/custom_worlds").unwrap();
        assert!(overlay < custom, "the job's worlds are mounted over the overlay: {a}");
        assert!(a.contains("--bind /data/worlds/1/out /job/out"), "{a}");
        assert_eq!(a.matches("--bind ").count(), 1, "{a}");
    }

    #[test]
    fn a_sidecar_without_a_tools_folder_is_an_error() {
        let mut s = sandbox();
        s.paths.sidecar = "/ap_gen.py".into();
        assert!(s.argv(false, &[], &[]).is_err());
    }

    #[test]
    fn the_error_tail_keeps_the_end_and_never_splits_a_character() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("aéb", 2), "b");
    }

    #[test]
    fn python_roots_follow_symlinks_and_skip_usr() {
        let dir = std::env::temp_dir().join(format!("sandbox-roots-{}", std::process::id()));
        let (base, venv) = (dir.join("base"), dir.join("venv"));
        std::fs::create_dir_all(base.join("bin")).unwrap();
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(base.join("bin/python3.12"), "").unwrap();
        let link = venv.join("bin/python");
        drop(std::fs::remove_file(&link));
        std::os::unix::fs::symlink(base.join("bin/python3.12"), &link).unwrap();
        assert_eq!(python_roots(&link), vec![venv, base]);
        assert!(python_roots(Path::new("/usr/bin/python3")).is_empty());
    }
}
