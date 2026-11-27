//! Uploaded world and player files: the kinds of upload, their records and files in the
//! host's store, and the id and name checks every upload path shares.

use super::*;

/// What stores a kind of upload: its folder and file extension (in a job folder, and under the
/// data folder of an older server), and which game names its record holds. Every kind is stored
/// alike, so a new kind is one entry here plus its route and a [`UploadKind`].
pub(super) struct KindSpec {
    pub(super) folder: &'static str,
    pub(super) ext: &'static str,
    /// The game names in a record (untrusted; [`game_folder`] makes the folder from them).
    pub(super) games: fn(&Value) -> Vec<&str>,
}

pub(super) const KINDS: [KindSpec; 2] = [
    KindSpec {
        folder: "apworlds",
        ext: "apworld",
        games: |meta| meta["game"].as_str().into_iter().collect(),
    },
    KindSpec {
        folder: "yamls",
        ext: "yaml",
        games: |meta| {
            let players = meta["players"].as_array().map_or(&[][..], Vec::as_slice);
            players.iter().filter_map(|p| p["game"].as_str()).collect()
        },
    },
];

/// The kinds of upload; the discriminant indexes [`KINDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Apworld = 0,
    Yaml = 1,
}

impl Kind {
    pub(super) const fn spec(self) -> &'static KindSpec {
        &KINDS[self as usize]
    }
    pub(super) const fn folder(self) -> &'static str {
        self.spec().folder
    }
    pub(super) const fn ext(self) -> &'static str {
        self.spec().ext
    }
    pub(super) const fn store(self) -> UploadKind {
        match self {
            Self::Apworld => UploadKind::Apworld,
            Self::Yaml => UploadKind::Yaml,
        }
    }
    pub(super) const fn cap(self, l: &Limits) -> usize {
        match self {
            Self::Apworld => l.apworld_max_bytes,
            Self::Yaml => l.yaml_max_bytes,
        }
    }
}

/// Longest game folder name.
pub(super) const MAX_SLUG: usize = 64;
/// The folder of a yaml that names several games.
pub(super) const MIXED: &str = "mixed";
/// The folder when an upload names no usable game.
pub(super) const UNKNOWN_GAME: &str = "unknown";

/// A game name from an upload (untrusted) as one folder name: lowercase `a-z0-9_-` only, so
/// never a `.`, a separator or an empty name, and at most [`MAX_SLUG`] long.
pub(super) fn game_slug(game: &str) -> String {
    let mut slug = String::new();
    for c in game.chars().flat_map(char::to_lowercase) {
        let c = if matches!(c, 'a'..='z' | '0'..='9' | '_') { c } else { '-' };
        if c != '-' || !(slug.is_empty() || slug.ends_with('-')) {
            slug.push(c);
        }
    }
    slug.truncate(MAX_SLUG);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { UNKNOWN_GAME.to_owned() } else { slug.to_owned() }
}

/// The game folder for an upload's record: its one game, or [`MIXED`] for several.
pub(super) fn game_folder(kind: Kind, meta: &Value) -> String {
    let mut games = (kind.spec().games)(meta);
    games.sort_unstable();
    games.dedup();
    match games[..] {
        [] => UNKNOWN_GAME.to_owned(),
        [one] => game_slug(one),
        _ => MIXED.to_owned(),
    }
}

impl Worlds {
    /// Upload `id`'s record, if stored.
    ///
    /// # Errors
    /// The store failing, or a record that is not JSON.
    pub(super) async fn upload_meta(&self, kind: Kind, id: &str) -> Result<Option<Value>, String> {
        let Some(key) = manifests::key_of(id) else { return Ok(None) };
        let meta = self.store.upload_meta(kind.store(), &key).await;
        let meta = meta.map_err(|e| store_failed(&e))?;
        meta.map(|m| serde_json::from_str(&m).map_err(|e| format!("upload {id}: bad record: {e}")))
            .transpose()
    }

    /// Upload `id`'s record when it is stored whole. A stored upload whose bytes no longer hash
    /// to its id (a torn row) is not answered as stored: the upload falls through to the check
    /// and is written again, which heals it, as a manifest is.
    ///
    /// # Errors
    /// The store failing.
    pub(super) async fn intact_upload(
        &self,
        kind: Kind,
        id: &str,
    ) -> Result<Option<Value>, String> {
        let Some(meta) = self.upload_meta(kind, id).await? else { return Ok(None) };
        let key = manifests::key_of(id).ok_or(format!("bad id {id}"))?;
        // Checked where the file is: its bytes (up to 64 MiB) do not travel.
        match self.store.upload_intact(kind.store(), &key).await {
            Ok(true) => Ok(Some(meta)),
            Ok(false) => {
                tracing::warn!(id, "stored upload is missing or torn; storing it again");
                Ok(None)
            }
            Err(e) => Err(store_failed(&e)),
        }
    }

    /// Upload `id`'s file, checked against its id.
    ///
    /// # Errors
    /// No such upload, or the store failing.
    pub(super) async fn upload_file(&self, kind: Kind, id: &str) -> Result<Vec<u8>, String> {
        let key = manifests::key_of(id).ok_or(format!("bad id {id}"))?;
        let file = self.store.upload_bytes(kind.store(), &key).await;
        file.map_err(|e| format!("upload {id}: {e:?}"))?.ok_or(format!("no upload {id}"))
    }
}

/// Ids are the lowercase hex of a SHA-256: checked before any id reaches a path.
pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The content address of an upload.
pub(super) fn content_id(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The `name` query parameter, percent-decoded, checked to be a plain file name. It is only
/// shown back (storage uses the content address).
pub(super) fn upload_name(query: Option<&str>) -> Result<String, String> {
    let raw =
        raw_param(query.unwrap_or(""), "name").ok_or("the name query parameter is required")?;
    let name = percent_decode(raw).ok_or("the name is not valid percent-encoded UTF-8")?;
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." {
        return Err("the name must be 1 to 255 bytes".to_owned());
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err("the name must be a plain file name".to_owned());
    }
    Ok(name)
}

/// The ids in `body[field]`: each valid, stored, and listed once. A bad choice is a 400 with
/// its reason; the store failing is a 500.
pub(super) async fn chosen(
    w: &Worlds,
    body: &Value,
    field: &str,
    kind: Kind,
) -> Result<Vec<Value>, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let ids = body.get(field).and_then(Value::as_array);
    let ids = ids.ok_or_else(|| bad(format!("{field} must be a list")))?;
    let mut metas = Vec::with_capacity(ids.len());
    for raw in ids {
        let id = raw.as_str().filter(|i| valid_id(i));
        let id = id.ok_or_else(|| bad(format!("{field}: bad id {raw}")))?;
        if metas.iter().any(|m: &Value| m["id"] == id) {
            return Err(bad(format!("{field}: {id} is listed twice")));
        }
        let meta = w.upload_meta(kind, id).await.map_err(internal)?;
        metas.push(meta.ok_or_else(|| bad(format!("{field}: no upload {id}")))?);
    }
    Ok(metas)
}
