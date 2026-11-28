//! The settings side of `rando` (design doc 20 section 8.2): `rando check FILE.randoset` and
//! `rando settings json FILE`, each with `--world PATH`. The world is found as section 5 says:
//! the `--world` flag (a folder holding `rando.toml`, or a `.randopack`), then the header's
//! `from` (relative to the settings file's folder), then the nearest folder above the file
//! with a `rando.toml`. The cache and the registry, the fourth place, are not built yet.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rando_limits::Limits;
use rando_settings::canon::{Presets, settle};
use rando_settings::schema::Schema;
use world_build::front::{Dir, Options};
use world_build::meta::FILE;

/// A settings file's extension.
pub const EXT: &str = "randoset";

/// Where the world came from: its folder, or its package's bytes.
enum Source {
    Folder(PathBuf),
    Pack(Vec<u8>),
}

/// The world's presets, from `presets/NAME.randoset` in its folder or `source/presets/` in its
/// package. Community presets need the registry client, not built yet.
struct Found(Source);

impl Presets for Found {
    fn world(&self, name: &str) -> Option<(String, String)> {
        let rel = format!("presets/{name}.{EXT}");
        let l = Limits::DESIGN;
        let text = match &self.0 {
            Source::Folder(d) => read_capped(&d.join(&rel), l.settings_max_bytes).ok()?,
            Source::Pack(bytes) => {
                entry(bytes, &format!("source/{rel}"), l.settings_max_bytes).ok()?
            }
        };
        Some((text, rel))
    }
    fn community(&self, _: &str, _: &str) -> Option<(String, String)> {
        None
    }
}

/// A file's text, refused past `cap` bytes.
fn read_capped(p: &Path, cap: usize) -> Result<String, String> {
    let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut s = String::new();
    f.take(cap as u64 + 1).read_to_string(&mut s).map_err(|e| format!("{}: {e}", p.display()))?;
    if s.len() > cap {
        return Err(format!("{} is over {cap} bytes", p.display()));
    }
    Ok(s)
}

/// Entry `name` of the package `bytes`, refused past `cap` bytes.
fn entry(bytes: &[u8], name: &str, cap: usize) -> Result<String, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let f = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut s = String::new();
    f.take(cap as u64 + 1).read_to_string(&mut s).map_err(|e| format!("{name}: {e}"))?;
    if s.len() > cap {
        return Err(format!("{name} is over {cap} bytes"));
    }
    Ok(s)
}

/// The schema of the world at `path`: a folder built in memory, or a package's `settings.json`.
fn load(path: &Path) -> Result<(Schema, Source), String> {
    let l = Limits::DESIGN;
    let (text, source) = if path.extension().is_some_and(|x| x == world_build::pack::EXT) {
        let cap = l.package_max_bytes;
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.len() > cap as u64 {
            return Err(format!("{} is over {cap} bytes", path.display()));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        (entry(&bytes, "settings.json", l.package_max_settings_bytes)?, Source::Pack(bytes))
    } else {
        let built =
            world_build::build(&Dir(path.to_path_buf()), &Options::default()).map_err(|list| {
                let all: Vec<String> = list.iter().map(ToString::to_string).collect();
                format!("the world at {} does not build:\n{}", path.display(), all.join("\n"))
            })?;
        (built.settings_schema()?, Source::Folder(path.to_path_buf()))
    };
    Ok((Schema::parse(&text, &l)?, source))
}

/// The world for settings file `file`, by section 5's order.
fn find(file: &Path, text: &str, flag: Option<&Path>) -> Result<(Schema, Source), String> {
    let l = Limits::DESIGN;
    let (parsed, _) = rando_settings::ast::parse(text, "", &l);
    let folder = file.parent().unwrap_or(Path::new("."));
    if let Some(p) = flag {
        return load(p);
    }
    if let Some(from) = &parsed.world.from {
        return load(&folder.join(&from.v));
    }
    let up = folder.ancestors().find(|d| d.join(FILE).is_file());
    match up {
        Some(d) => load(d),
        None => {
            let name = parsed.world.name.as_ref().map_or("this world", |n| n.v.as_str());
            Err(format!(
                "No world '{name}' was found: there is no `--world`, no `from` in the header and no {FILE} in a folder above the file. Give `--world PATH` (a world folder or a .randopack); the registry lookup is not built yet."
            ))
        }
    }
}

/// `rando check FILE.randoset` (`json` false) or `rando settings json FILE` (`json` true).
pub fn run(file: &Path, world: Option<&Path>, json: bool) -> ExitCode {
    let l = Limits::DESIGN;
    let name = file
        .file_name()
        .map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned());
    let text = match read_capped(file, l.settings_max_bytes) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e} (settings_max_bytes)");
            return ExitCode::FAILURE;
        }
    };
    let (schema, source) = match find(file, &text, world) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: {name}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match settle(&text, &name, &schema, &Found(source), &l) {
        Ok(s) => {
            if json {
                print!("{}", s.canonical);
                println!("{}", s.hash);
            }
            ExitCode::SUCCESS
        }
        Err(list) => {
            for e in &list {
                eprintln!("error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

/// `rando check`'s or `rando settings json`'s arguments: the file and `--world PATH`.
pub fn args(rest: &[String]) -> Option<(PathBuf, Option<PathBuf>)> {
    let (mut file, mut world) = (None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--world" => world = Some(PathBuf::from(it.next()?)),
            s if s.starts_with('-') => return None,
            s if file.is_none() => file = Some(PathBuf::from(s)),
            _ => return None,
        }
    }
    Some((file?, world))
}
