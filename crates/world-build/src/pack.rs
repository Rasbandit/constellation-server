//! `rando pack` (design doc 19 sections 3 and 4): a world folder to one `.randopack`, a zip of
//! `manifest.json`, `meta.json`, `settings.json`, `assets/`, optional `source/` and `sha256.txt`. The same
//! folder gives the same bytes with the same `rando`: entries in path order, one fixed time
//! and mode, one compression level. Nothing is written here; the caller writes the package,
//! then the lock.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::front::{Dir, Files, LEDGER, Options};
use crate::meta::{FILE, Meta};

/// The package's file extension (a working name, doc 19 section 3).
pub const EXT: &str = "randopack";

/// A package made in memory.
#[derive(Debug, Clone)]
pub struct Packed {
    /// `<name>-<version>.randopack`.
    pub file_name: String,
    /// The zip.
    pub bytes: Vec<u8>,
    /// The lock after this build, its ids marked published: write it once the package is
    /// written.
    pub lock: String,
}

/// Every file a build reads, kept as read, so `source/` holds exactly what the build used.
struct Recording<'a> {
    inner: &'a dyn Files,
    read: RefCell<BTreeMap<String, String>>,
}

impl Files for Recording<'_> {
    fn read(&self, path: &str) -> Option<Result<String, String>> {
        let r = self.inner.read(path);
        if let Some(Ok(t)) = &r {
            self.read.borrow_mut().insert(path.to_owned(), t.clone());
        }
        r
    }
}

/// A PNG or WebP file, by its first bytes, not its name.
fn image(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n")
        || (b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP")
}

/// Plain text: UTF-8 with no NUL and no control characters but tab, line feed and carriage
/// return.
fn text(b: &[u8]) -> bool {
    std::str::from_utf8(b)
        .is_ok_and(|t| t.chars().all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r')))
}

/// Pack the world in `dir`, with its source when `source` (required to publish on the portal;
/// leave it out only for local use).
///
/// # Errors
/// Every reason it was refused, one line each, printed as `rando check` prints them.
pub fn pack(dir: &Dir, source: bool, l: &rando_limits::Limits) -> Result<Packed, Vec<String>> {
    if dir.read(FILE).is_none() {
        return Err(vec![format!(
            "rando pack requires a {FILE} in {}; see design doc 19 section 2",
            dir.0.display()
        )]);
    }
    let files = Recording { inner: dir, read: RefCell::new(BTreeMap::new()) };
    let opts = Options { publish: true, ..Options::default() };
    let built = crate::build(&files, &opts)
        .map_err(|e| e.iter().map(|e| format!("error: {e}")).collect::<Vec<_>>())?;
    if !built.warnings.is_empty() {
        let mut out: Vec<String> = built.warnings.iter().map(|w| format!("warning: {w}")).collect();
        out.push("publishing refuses a world with warnings".into());
        return Err(out);
    }
    let (Some(meta), Some(lock)) = (built.meta.clone(), built.ledger.clone()) else {
        return Err(vec![format!("{FILE} was not read")]);
    };
    let mut errs = Vec::new();
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let manifest_max = l.manifest_max_bytes;
    if built.json().len() > manifest_max {
        errs.push(format!("the manifest is over {manifest_max} bytes"));
    }
    entries.insert("manifest.json".into(), built.json().as_bytes().to_vec());
    let settings = built.settings_schema().map_err(|e| vec![format!("error: {e}")])?;
    if settings.len() > l.package_max_settings_bytes {
        errs.push(format!("settings.json is over {} bytes", l.package_max_settings_bytes));
    }
    entries.insert("settings.json".into(), settings.into_bytes());
    let license_file = assets(dir, &meta, l, &mut entries, &mut errs);
    let meta_json = crate::canon::to_string(&meta_json(&meta, license_file))
        .map_err(|e| vec![format!("meta.json: {e}")])?;
    entries.insert("meta.json".into(), meta_json.into_bytes());
    if source {
        let mut total = 0;
        // `rando.toml`, the modules and the table files; the lock is the one this build wrote.
        let kept =
            files.read.into_inner().into_iter().filter(|(p, _)| {
                p == FILE || p.ends_with(crate::front::EXT) || p.ends_with(".csv")
            });
        for (p, t) in kept {
            total += t.len();
            entries.insert(format!("source/{p}"), t.into_bytes());
        }
        total += lock.len();
        entries.insert(format!("source/{LEDGER}"), lock.clone().into_bytes());
        if total > l.package_max_source_bytes {
            errs.push(format!("the source is over {} bytes", l.package_max_source_bytes));
        }
    }
    let sums: String = entries
        .iter()
        .map(|(p, b)| format!("{}  {p}\n", crate::front::hex(&Sha256::digest(b))))
        .collect();
    entries.insert("sha256.txt".into(), sums.into_bytes());
    if entries.len() > l.package_max_entries {
        errs.push(format!("more than {} entries", l.package_max_entries));
    }
    for p in entries.keys().filter(|p| p.len() > l.package_max_path_bytes) {
        errs.push(format!("{p} is a path over {} bytes", l.package_max_path_bytes));
    }
    if !errs.is_empty() {
        return Err(errs.into_iter().map(|e| format!("error: {e}")).collect());
    }
    let bytes = zip(&entries).map_err(|e| vec![format!("error: writing the package: {e}")])?;
    if bytes.len() > l.package_max_bytes {
        return Err(vec![format!("error: the package is over {} bytes", l.package_max_bytes)]);
    }
    Ok(Packed { file_name: format!("{}-{}.{EXT}", meta.name, meta.version), bytes, lock })
}

/// The icon, the `[assets]` files and a `LICENSE` for a `LicenseRef-` license, each checked,
/// into `assets/`; returns the `LICENSE` entry when there is one.
fn assets(
    dir: &Dir,
    meta: &Meta,
    l: &rando_limits::Limits,
    entries: &mut BTreeMap<String, Vec<u8>>,
    errs: &mut Vec<String>,
) -> Option<String> {
    let license_ref = meta.license.as_ref().is_some_and(|l| l.contains("LicenseRef-"));
    let mut want: Vec<(&str, usize)> = Vec::new();
    want.extend(meta.icon.iter().map(|i| (i.as_str(), l.world_icon_max_bytes)));
    want.extend(meta.assets.iter().map(|a| (a.as_str(), l.world_asset_max_bytes)));
    if license_ref {
        want.push(("LICENSE", l.world_asset_max_bytes));
    }
    let mut total = 0;
    for (path, max) in want {
        match dir.read_bytes(path, max) {
            None if path == "LICENSE" => errs.push(format!(
                "the license {:?} is a LicenseRef, which needs a LICENSE file in the world folder",
                meta.license.as_deref().unwrap_or_default()
            )),
            None => errs.push(format!("{path} does not exist")),
            Some(Err(e)) => errs.push(e),
            Some(Ok(b)) if meta.icon.as_deref() == Some(path) && !image(&b) => {
                errs.push(format!("{path}: the icon must be a PNG or WebP file"));
            }
            Some(Ok(b)) if !image(&b) && !text(&b) => {
                errs.push(format!("{path} is not a PNG, WebP or plain-text file"));
            }
            Some(Ok(b)) => {
                total += b.len();
                entries.insert(format!("assets/{path}"), b);
            }
        }
    }
    if total > l.world_assets_max_bytes {
        errs.push(format!("the assets are over {} bytes in all", l.world_assets_max_bytes));
    }
    license_ref.then(|| "assets/LICENSE".to_owned())
}

/// `meta.json`: `rando.toml` normalised, defaults filled in, paths under `assets/`, and the
/// tool that built it.
fn meta_json(m: &Meta, license_file: Option<String>) -> Value {
    let asset = |p: &String| format!("assets/{p}");
    let mut w = json!({
        "name": m.name,
        "version": m.version,
        "entry": m.entry,
        "authors": m.authors,
    });
    for (k, v) in [
        ("license", m.license.clone()),
        ("description", m.description.clone()),
        ("icon", m.icon.as_ref().map(asset)),
        ("homepage", m.homepage.clone()),
        ("repository", m.repository.clone()),
        ("license_file", license_file),
    ] {
        if let Some(v) = v {
            w[k] = json!(v);
        }
    }
    let files: Vec<String> = m.assets.iter().map(asset).collect();
    json!({
        "assets": {"files": files},
        "tool": format!("rando {}", env!("CARGO_PKG_VERSION")),
        "world": w,
    })
}

/// The zip: entries in path order, files only, the zip epoch for every time, mode 0644,
/// deflate at one level, no extra fields, no comment.
fn zip(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9))
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (p, b) in entries {
        w.start_file(p.as_str(), opts).map_err(|e| e.to_string())?;
        w.write_all(b).map_err(|e| e.to_string())?;
    }
    Ok(w.finish().map_err(|e| e.to_string())?.into_inner())
}
