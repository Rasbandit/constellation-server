//! The settings schema (`settings.json`, design doc 20 section 5.1) a file is checked against,
//! and the common vocabulary of doc 15 section 4, which the platform owns and this crate ships
//! (doc 20 section 2.5).
//!
//! A reader of `schema/1` ignores keys it does not know (the writer's rule, `world-build`'s
//! `schema.rs`), so a key the build tool adds later still reads.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use rando_limits::Limits;
use serde_json::{Map, Value, json};

/// The `format` this reader knows.
pub const FORMAT: &str = "schema/1";

/// What a list option's names are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Of {
    /// The world's own words.
    Words(Vec<String>),
    /// Item names and item groups.
    Items,
    /// Place names and place groups.
    Places,
    /// Item link names, which the room declares (`shared_items`, doc 20 section 3); any name.
    Links,
}

/// An option's kind (doc 20 section 5.1, `kind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `on` or `off`.
    Toggle,
    /// A whole number in `min..=max`, or a named number.
    Range {
        /// The lowest.
        min: i64,
        /// The highest.
        max: i64,
        /// Named numbers; a world may name one outside the bounds.
        named: BTreeMap<String, i64>,
    },
    /// One of `values`; with `or_text`, a string of at most that many bytes too.
    Choice {
        /// The values, in declared order.
        values: Vec<String>,
        /// The longest text, for a choice-or-text option.
        or_text: Option<usize>,
    },
    /// A string of at most `max_len` bytes.
    Text {
        /// The longest.
        max_len: usize,
    },
    /// A list of names; `copies` when its entries take `x N`.
    Set {
        /// Its names.
        of: Of,
        /// The fewest and most entries, when declared.
        count: Option<(u64, u64)>,
        /// Entries take counts (`start_with`'s lists).
        copies: bool,
    },
    /// One whole number per name, each in `min..=max`.
    Counts {
        /// The names.
        of: Vec<String>,
        /// The lowest.
        min: i64,
        /// The highest.
        max: i64,
    },
    /// A one-level record of fields (`start_with`).
    Record(Vec<(String, Kind)>),
    /// A key the room host decides (doc 15 section 4, "R"): an error in a player file.
    Room,
}

impl Kind {
    /// A list kind, which takes `some_of` (2.10).
    #[must_use]
    pub fn is_list(&self) -> bool {
        matches!(self, Kind::Set { .. })
    }
}

/// What an option changes (doc 20 section 2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The game.
    Seed,
    /// What players know.
    Knowledge,
    /// Only looks and sound: drawn on the device and left out of the hash.
    Cosmetic,
}

/// One option.
#[derive(Debug, Clone, PartialEq)]
pub struct Opt {
    /// Its key.
    pub key: String,
    /// Its kind.
    pub kind: Kind,
    /// Its label in English, when it has one.
    pub label: Option<String>,
    /// Its scope.
    pub scope: Scope,
    /// The screen section it shows in, when declared.
    pub section: Option<String>,
    /// Its default, in canonical JSON form.
    pub default: Value,
    /// A common key the world must `offer` (doc 20 section 2.5).
    pub needs_offer: bool,
    /// Other names players know it by (doc 15 section 4, "Community aliases").
    pub aliases: &'static [&'static str],
}

/// One item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Item {
    /// Its copies in the shuffle, when they do not read options.
    pub copies: Option<u64>,
    /// It is filler.
    pub filler: bool,
}

/// One world shuffle (doc 17): what `pin` places into which slots.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shuffle {
    /// Its things.
    pub things: BTreeSet<String>,
    /// Its slots.
    pub slots: BTreeSet<String>,
}

/// A world's settings schema, with the common vocabulary added.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    /// The world's name.
    pub world: String,
    /// Its exact version.
    pub version: String,
    /// Every option, common keys included, by key.
    pub options: BTreeMap<String, Opt>,
    /// The world's own option keys in declared order, then the common keys.
    pub order: Vec<String>,
    /// Items by name.
    pub items: BTreeMap<String, Item>,
    /// Place names.
    pub places: BTreeSet<String>,
    /// Item groups and their members.
    pub item_groups: BTreeMap<String, Vec<String>>,
    /// Place groups and their members.
    pub place_groups: BTreeMap<String, Vec<String>>,
    /// Door names.
    pub doors: BTreeSet<String>,
    /// Shuffles by name.
    pub shuffles: BTreeMap<String, Shuffle>,
    /// The common keys needing the game's help that the world offers.
    pub offers: BTreeSet<String>,
    /// The world's presets, each with its token hash when the schema gives one.
    pub presets: BTreeMap<String, Option<String>>,
    /// The world runs in the compatibility sidecar, so `legacy` changes the game (doc 20
    /// section 6). Read from an optional `sidecar` key.
    pub sidecar: bool,
}

/// The built-in item groups (doc 20 section 2.13).
pub const EVERY_ITEM: &str = "every_item";
/// The built-in place group of every place.
pub const EVERY_PLACE: &str = "every_place";
/// The places reachable from the start, known per seed.
pub const EARLY_PLACES: &str = "early_places";
/// Every other place, known per seed.
pub const LATER_PLACES: &str = "later_places";

/// The screen section of the common keys (doc 15 section 2.5).
const WITH_OTHERS: &str = "Playing with others";

/// The common vocabulary (doc 15 section 4, as changed by doc 20 sections 2.5 and 3): the
/// player keys, then the room keys a player file may not set. The "G" keys exist only when a
/// world declares them.
fn common() -> Vec<Opt> {
    let opt = |key: &str, label: &str, kind: Kind, default: Value| Opt {
        key: key.to_owned(),
        kind,
        label: Some(label.to_owned()),
        scope: Scope::Seed,
        section: Some(WITH_OTHERS.to_owned()),
        default,
        needs_offer: false,
        aliases: &[],
    };
    let choice = |v: &[&str]| Kind::Choice {
        values: v.iter().map(|s| (*s).to_owned()).collect(),
        or_text: None,
    };
    let set = |of: Of| Kind::Set { of, count: None, copies: false };
    let copies = Kind::Set { of: Of::Items, count: None, copies: true };
    let mut out = Vec::from([
        Opt {
            aliases: &["balancing", "progression_balancing"],
            ..opt(
                "keep_moving",
                "Keep everyone moving",
                choice(&["off", "gentle", "strong"]),
                json!("gentle"),
            )
        },
        Opt {
            aliases: &["accessibility"],
            ..opt(
                "must_reach",
                "What must be reachable",
                choice(&["everything", "all_items", "just_the_goal"]),
                json!("everything"),
            )
        },
        Opt {
            aliases: &["local_items"],
            ..opt("keep_home", "Keep these items in my game", set(Of::Items), json!([]))
        },
        Opt {
            aliases: &["non_local_items"],
            ..opt("send_away", "Send these items to other players", set(Of::Items), json!([]))
        },
        Opt {
            aliases: &["start_inventory", "starting_items"],
            ..opt(
                "start_with",
                "Start with these items",
                Kind::Record(Vec::from([
                    ("from_shuffle".to_owned(), copies.clone()),
                    ("items".to_owned(), copies),
                ])),
                json!({"from_shuffle": [], "items": []}),
            )
        },
        Opt {
            scope: Scope::Knowledge,
            aliases: &["start_hints"],
            ..opt("reveal_items", "Show where these items are", set(Of::Items), json!([]))
        },
        Opt {
            scope: Scope::Knowledge,
            aliases: &["start_location_hints"],
            ..opt("reveal_places", "Show what is at these places", set(Of::Places), json!([]))
        },
        Opt {
            aliases: &["exclude_locations"],
            ..opt("no_important_at", "Never put important items here", set(Of::Places), json!([]))
        },
        Opt {
            aliases: &["priority_locations"],
            ..opt("important_at", "Put important items here", set(Of::Places), json!([]))
        },
        Opt {
            aliases: &["item_links"],
            ..opt("shared_items", "Share items with teammates", set(Of::Links), json!([]))
        },
        Opt {
            needs_offer: true,
            aliases: &["deathlink", "death_link"],
            ..opt("shared_deaths", "Shared deaths", Kind::Toggle, json!(false))
        },
    ]);
    for (key, label) in [
        ("hint_price", "Hint price"),
        ("spoilers", "Spoiler log"),
        ("on_finish", "When a player finishes"),
        ("allow_shared_deaths", "Allow shared deaths"),
        ("allow_surprise", "Allow surprises"),
        ("allow_place", "Allow hand placement"),
        ("allow_connect", "Allow hand connections"),
    ] {
        out.push(opt(key, label, Kind::Room, Value::Null));
    }
    out
}

fn str_list(v: &Value, what: &str) -> Result<Vec<String>, String> {
    v.as_array()
        .ok_or_else(|| format!("{what} is not a list"))?
        .iter()
        .map(|x| {
            x.as_str().map(ToOwned::to_owned).ok_or_else(|| format!("{what} holds a non-string"))
        })
        .collect()
}

/// `[{"name": ...}, ...]`, the shape every schema list has.
/// A schema list's entries: each name and its object.
type Named = Vec<(String, Map<String, Value>)>;

fn names(v: Option<&Value>, what: &str) -> Result<Named, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let list = v.as_array().ok_or_else(|| format!("`{what}` is not a list"))?;
    list.iter()
        .map(|x| {
            let o = x.as_object().ok_or_else(|| format!("`{what}` holds a non-object"))?;
            let n = o
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("`{what}` holds an entry with no name"))?;
            Ok((n.to_owned(), o.clone()))
        })
        .collect()
}

fn int(o: &Map<String, Value>, k: &str, key: &str) -> Result<i64, String> {
    o.get(k).and_then(Value::as_i64).ok_or_else(|| format!("option `{key}` has no integer `{k}`"))
}

fn usize_of(o: &Map<String, Value>, k: &str, key: &str) -> Result<usize, String> {
    let n =
        o.get(k).and_then(Value::as_u64).ok_or_else(|| format!("option `{key}` has no `{k}`"))?;
    usize::try_from(n).map_err(|_| format!("option `{key}`: `{k}` is too large"))
}

/// A world option's kind and canonical default.
fn option_kind(o: &Map<String, Value>, key: &str) -> Result<(Kind, Value), String> {
    let kind = o.get("kind").and_then(Value::as_str).unwrap_or("");
    let default =
        o.get("default").cloned().ok_or_else(|| format!("option `{key}` has no `default`"))?;
    let of = |o: &Map<String, Value>| -> Result<Of, String> {
        match o.get("of") {
            Some(Value::String(s)) if s == "items" => Ok(Of::Items),
            Some(Value::String(s)) if s == "places" => Ok(Of::Places),
            Some(v @ Value::Array(_)) => Ok(Of::Words(str_list(v, "`of`")?)),
            _ => Err(format!("option `{key}` has no `of`")),
        }
    };
    let k = match kind {
        "toggle" => Kind::Toggle,
        "range" => {
            let mut named = BTreeMap::new();
            if let Some(m) = o.get("named").and_then(Value::as_object) {
                for (n, v) in m {
                    let v = v.as_i64().ok_or_else(|| {
                        format!("option `{key}`: named number `{n}` is not an integer")
                    })?;
                    named.insert(n.clone(), v);
                }
            }
            Kind::Range { min: int(o, "min", key)?, max: int(o, "max", key)?, named }
        }
        "choice" => {
            let values = str_list(o.get("values").unwrap_or(&Value::Null), "`values`")?;
            let or_text = if o.get("or_text").and_then(Value::as_bool) == Some(true) {
                Some(usize_of(o, "max_len", key)?)
            } else {
                None
            };
            Kind::Choice { values, or_text }
        }
        "text" => Kind::Text { max_len: usize_of(o, "max_len", key)? },
        "set" => {
            let count = match o.get("count").and_then(Value::as_array).map(Vec::as_slice) {
                Some([a, b]) => Some((
                    a.as_u64().ok_or_else(|| format!("option `{key}`: bad `count`"))?,
                    b.as_u64().ok_or_else(|| format!("option `{key}`: bad `count`"))?,
                )),
                None => None,
                Some(_) => return Err(format!("option `{key}`: `count` is [lowest, highest]")),
            };
            Kind::Set { of: of(o)?, count, copies: false }
        }
        "counts" => {
            let Of::Words(w) = of(o)? else {
                return Err(format!("option `{key}`: a counts option names its own words"));
            };
            Kind::Counts { of: w, min: int(o, "min", key)?, max: int(o, "max", key)? }
        }
        k => return Err(format!("option `{key}` has an unknown kind `{k}`")),
    };
    // Lists are sets: canonical JSON sorts them.
    let default = match (&k, default) {
        (Kind::Set { .. }, Value::Array(mut a)) => {
            a.sort_by(|x, y| x.as_str().cmp(&y.as_str()));
            Value::Array(a)
        }
        (_, d) => d,
    };
    Ok((k, default))
}

impl Schema {
    /// `text`, a `settings.json`, read and joined with the common vocabulary.
    ///
    /// # Errors
    /// Text over `package_max_settings_bytes`, not JSON, another `format`, or a field of the
    /// wrong shape, as a message.
    pub fn parse(text: &str, l: &Limits) -> Result<Schema, String> {
        if text.len() > l.package_max_settings_bytes {
            return Err(format!(
                "settings.json is {} bytes; at most {} (package_max_settings_bytes)",
                text.len(),
                l.package_max_settings_bytes
            ));
        }
        let v: Value = serde_json::from_str(text).map_err(|e| format!("settings.json: {e}"))?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(ToOwned::to_owned);
        match s("format") {
            Some(f) if f == FORMAT => {}
            f => {
                return Err(format!(
                    "settings.json has format {f:?}; this reader knows {FORMAT:?}"
                ));
            }
        }
        let world = s("world").ok_or("settings.json has no `world`")?;
        let version = s("version").ok_or("settings.json has no `version`")?;
        let label = |k: &str| {
            v.get("strings")
                .and_then(|t| t.get(k))
                .and_then(|t| t.get("en"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        };
        let mut options = BTreeMap::new();
        let mut order = Vec::new();
        for o in v.get("options").and_then(Value::as_array).into_iter().flatten() {
            let o = o.as_object().ok_or("an option is not an object")?;
            let key =
                o.get("key").and_then(Value::as_str).ok_or("an option has no key")?.to_owned();
            let (kind, default) = option_kind(o, &key)?;
            let scope = match o.get("scope").and_then(Value::as_str) {
                None | Some("seed") => Scope::Seed,
                Some("knowledge") => Scope::Knowledge,
                Some("cosmetic") => Scope::Cosmetic,
                Some(x) => return Err(format!("option `{key}` has an unknown scope `{x}`")),
            };
            let opt = Opt {
                label: o.get("label").and_then(Value::as_str).and_then(label),
                section: o.get("section").and_then(Value::as_str).map(ToOwned::to_owned),
                key: key.clone(),
                kind,
                scope,
                default,
                needs_offer: false,
                aliases: &[],
            };
            if options.insert(key.clone(), opt).is_some() {
                return Err(format!("option `{key}` is declared twice"));
            }
            order.push(key);
        }
        for c in common() {
            if options.contains_key(&c.key) {
                return Err(format!(
                    "the world declares `{}`, a common key the platform owns",
                    c.key
                ));
            }
            order.push(c.key.clone());
            options.insert(c.key.clone(), c);
        }
        let mut items = BTreeMap::new();
        for (n, o) in names(v.get("items"), "items")? {
            let item = Item {
                copies: o.get("copies").and_then(Value::as_u64),
                filler: o.get("filler").and_then(Value::as_bool) == Some(true),
            };
            items.insert(n, item);
        }
        let groups = |k: &str| -> Result<BTreeMap<String, Vec<String>>, String> {
            names(v.get(k), k)?
                .into_iter()
                .map(|(n, o)| {
                    Ok((n, str_list(o.get("members").unwrap_or(&Value::Null), "`members`")?))
                })
                .collect()
        };
        let mut shuffles = BTreeMap::new();
        for (n, o) in names(v.get("shuffles"), "shuffles")? {
            let set = |k: &str| -> Result<BTreeSet<String>, String> {
                Ok(str_list(o.get(k).unwrap_or(&json!([])), k)?.into_iter().collect())
            };
            shuffles.insert(n, Shuffle { things: set("things")?, slots: set("slots")? });
        }
        Ok(Schema {
            world,
            version,
            options,
            order,
            items,
            places: names(v.get("places"), "places")?.into_iter().map(|p| p.0).collect(),
            item_groups: groups("item_groups")?,
            place_groups: groups("place_groups")?,
            doors: names(v.get("doors"), "doors")?.into_iter().map(|p| p.0).collect(),
            shuffles,
            offers: str_list(v.get("offers").unwrap_or(&json!([])), "`offers`")?
                .into_iter()
                .collect(),
            presets: names(v.get("presets"), "presets")?
                .into_iter()
                .map(|(n, o)| (n, o.get("hash").and_then(Value::as_str).map(ToOwned::to_owned)))
                .collect(),
            sidecar: v.get("sidecar").and_then(Value::as_bool) == Some(true),
        })
    }

    /// An option's label, or its key in quotes when it has none.
    #[must_use]
    pub fn label(&self, key: &str) -> String {
        match self.options.get(key).and_then(|o| o.label.as_ref()) {
            Some(l) => l.clone(),
            None => key.to_string(),
        }
    }

    /// The world's name as players read it: the registry name, capitalised.
    #[must_use]
    pub fn title(&self) -> String {
        let mut c = self.world.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata::{CINDERFALL, LANTERNFALL};

    const L: Limits = Limits::DESIGN;

    #[test]
    fn the_golden_schema_reads_with_the_common_vocabulary() {
        let s = Schema::parse(CINDERFALL, &L).unwrap();
        assert_eq!((s.world.as_str(), s.version.as_str()), ("cinderfall", "1.0.0"));
        assert_eq!(s.order[..4], ["difficulty", "relics_needed", "sunken_vault", "start_lantern"]);
        let d = &s.options["difficulty"];
        assert_eq!(d.label.as_deref(), Some("Difficulty"));
        assert!(matches!(&d.kind, Kind::Choice { values, or_text: None } if values.len() == 3));
        assert_eq!(d.default, json!("standard"));
        assert!(matches!(s.options["relics_needed"].kind, Kind::Range { min: 1, max: 4, .. }));
        assert_eq!(s.items["blade"].copies, Some(3));
        assert!(s.items["coin"].filler);
        assert!(s.places.contains("anvil") && s.doors.contains("mill_door"));
        assert!(s.options["shared_deaths"].needs_offer && s.offers.is_empty());
        assert_eq!(s.options["hint_price"].kind, Kind::Room);
    }

    #[test]
    fn every_kind_of_section_5_1_reads() {
        let s = Schema::parse(LANTERNFALL, &L).unwrap();
        assert!(
            matches!(&s.options["bombs"].kind, Kind::Range { named, .. } if named["plenty"] == 99)
        );
        assert!(
            matches!(&s.options["shops"].kind, Kind::Set { of: Of::Words(w), count: Some((1, 3)), .. } if w.len() == 3)
        );
        assert!(matches!(&s.options["extra_tools"].kind, Kind::Set { of: Of::Items, .. }));
        assert!(matches!(&s.options["trap_mix"].kind, Kind::Counts { min: 0, max: 100, .. }));
        assert_eq!(s.options["greeting"].kind, Kind::Text { max_len: 20 });
        assert!(matches!(&s.options["title"].kind, Kind::Choice { or_text: Some(16), .. }));
        assert_eq!(s.options["lamp_color"].scope, Scope::Cosmetic);
        assert_eq!(s.item_groups["tools"], ["boots", "pickaxe", "rope"]);
        assert_eq!(s.place_groups["cave_places"].len(), 2);
        assert!(s.shuffles["bosses"].slots.contains("mill_arena"));
        assert!(s.offers.contains("shared_deaths"));
        assert!(s.presets.contains_key("relaxed"));
    }

    #[test]
    fn a_bad_schema_is_refused() {
        assert!(Schema::parse("{", &L).is_err());
        assert!(Schema::parse(r#"{"format":"schema/2","world":"w","version":"1"}"#, &L).is_err());
        let common = r#"{"format":"schema/1","world":"w","version":"1","options":[{"key":"keep_home","kind":"toggle","default":false}]}"#;
        assert!(Schema::parse(common, &L).unwrap_err().contains("common key"));
        let small = Limits { package_max_settings_bytes: 10, ..L };
        assert!(
            Schema::parse(CINDERFALL, &small).unwrap_err().contains("package_max_settings_bytes")
        );
    }
}
