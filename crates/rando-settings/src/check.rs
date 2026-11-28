//! The checker (design doc 20 section 7): one parsed file against a world's schema. Every
//! value is checked by its option's kind, and comes out in its canonical JSON form (section
//! 6), ready for layering ([`crate::canon`]). Every diagnostic carries its section 7.1 key.
//!
//! What needs every layer at once (a block changing what an earlier block tested, placements
//! that clash across presets, `send_away` against `place`) is checked in [`crate::canon`].

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use config::Limits;
use rando_syntax::{Diagnostic, Pos, suggest};
use serde_json::{Map, Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::ast::{
    Count, Dir, Entry, File, Header, Items, Line, Mod, Node, Op, PresetRef, Size, Stmt, Value as V,
};
use crate::diag;
use crate::schema::{
    EARLY_PLACES, EVERY_ITEM, EVERY_PLACE, Kind, LATER_PLACES, Of, Opt, Schema, Scope,
};

/// The settings format this crate reads (doc 20 section 11.1).
pub const FORMAT: i64 = 1;

/// A checked value: plain, or a surprise kept as its spec (doc 20 section 6).
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// A plain value in canonical form.
    Plain(Value),
    /// A surprise spec: `{"any":{}}`, `{"one_of":[...]}` and the rest.
    Surprise(Value),
}

/// One top-level setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    /// Its value.
    pub val: Val,
    /// Where the line is.
    pub pos: Pos,
}

/// One change line inside a block.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The option.
    pub key: String,
    /// `=`, `+=` or `-=`.
    pub op: Op,
    /// The value, or for `+=` and `-=` the names or counts to add or remove.
    pub val: Val,
    /// Where the line is.
    pub pos: Pos,
}

/// One `when` or `with` block (doc 20 section 2.12).
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The conditions, each an option and its canonical value; empty for `with`.
    pub when: Vec<(String, Value)>,
    /// The chance, when written.
    pub chance: Option<i64>,
    /// `with N%`.
    pub with: bool,
    /// The changes.
    pub changes: Vec<Change>,
    /// Every option it tests or changes is cosmetic.
    pub cosmetic: bool,
    /// Where it starts.
    pub pos: Pos,
}

/// One `place` (doc 20 section 2.11), in canonical form, with what the cross-layer checks
/// need.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// The canonical record.
    pub json: Value,
    /// The items it takes from the shuffle, with their counts (`None` for `x all`), when it
    /// names them rather than drawing them.
    pub items: Vec<(String, Option<i64>)>,
    /// Its one place, when it has exactly one candidate.
    pub single: Option<String>,
    /// Without `if_possible`.
    pub strict: bool,
    /// With `copy`.
    pub copy: bool,
    /// Where the line is.
    pub pos: Pos,
}

/// One checked layer: a player or preset file, its values in canonical form.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layer {
    /// The file's name, for messages.
    pub file: String,
    /// The preset's name, for a preset file.
    pub preset: Option<String>,
    /// The `player` line.
    pub player: Option<String>,
    /// The `preset` lines.
    pub presets: Vec<Node<PresetRef>>,
    /// The top-level settings by key.
    pub values: BTreeMap<String, Setting>,
    /// The blocks, in file order.
    pub blocks: Vec<Block>,
    /// The `place` lines.
    pub place: Vec<Placement>,
    /// The `connect` lines, canonical, each with its doors and place.
    pub connect: Vec<(Value, Vec<String>, Pos)>,
    /// The `pin` lines, canonical, each with its slot and place.
    pub pin: Vec<(Value, String, Pos)>,
    /// The `legacy` block.
    pub legacy: Map<String, Value>,
}

/// `f`, the file named `file`, checked against `s`: its layer and every error.
#[must_use]
pub fn check(f: &File, s: &Schema, file: &str, l: &Limits) -> (Layer, Vec<Diagnostic>) {
    let mut c = C { s, file, l, errs: Vec::new(), expanded: 0, copies: 0 };
    let mut layer = Layer { file: file.to_owned(), presets: f.presets.clone(), ..Layer::default() };
    c.header(f, &mut layer);
    for line in &f.body {
        c.line(line, &mut layer);
    }
    (layer, c.errs)
}

struct C<'a> {
    s: &'a Schema,
    file: &'a str,
    l: &'a Limits,
    errs: Vec<Diagnostic>,
    expanded: usize,
    copies: u64,
}

/// Unicode general category Cf (format), Unicode 15.1: the bidirectional overrides, zero-width
/// characters and the rest a player name may not hold (doc 20 section 7.1).
const FORMAT_CHARS: &[(u32, u32)] = &[
    (0xAD, 0xAD),
    (0x600, 0x605),
    (0x61C, 0x61C),
    (0x6DD, 0x6DD),
    (0x70F, 0x70F),
    (0x890, 0x891),
    (0x8E2, 0x8E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
];

fn is_format_char(c: char) -> bool {
    let n = u32::from(c);
    FORMAT_CHARS.iter().any(|(a, b)| (*a..=*b).contains(&n))
}

/// A string in canonical form: NFC (doc 20 section 6).
#[must_use]
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// `a, b or c`.
fn or_list(v: &[String]) -> String {
    match v {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn shape(v: &V) -> &'static str {
    match v {
        V::Name(_) => "a name",
        V::Int(_) => "a number",
        V::Str(_) => "a string",
        V::List(_) => "a list",
        V::Record(_) => "a record",
        _ => "a surprise",
    }
}

fn spec_key(v: &Value) -> String {
    rando_syntax::canon::to_string(v).unwrap_or_default()
}

impl C<'_> {
    fn err(&mut self, key: &str, pos: Pos, msg: String, help: Option<String>) {
        self.errs.push(diag(self.file, key, Some(pos), msg, help));
    }

    fn label(&self, opt: &Opt) -> String {
        opt.label.clone().unwrap_or_else(|| opt.key.clone())
    }

    fn header(&mut self, f: &File, layer: &mut Layer) {
        if let Some(n) = &f.format
            && n.v != FORMAT
        {
            let msg = if n.v > FORMAT {
                format!("This file uses settings format {}. Update rando to read it.", n.v)
            } else {
                format!("There is no settings format {}; this tool reads format {FORMAT}.", n.v)
            };
            self.err("settings.format", n.pos, msg, None);
        }
        let Some(h) = &f.header else { return };
        match &h.v {
            Header::Room => {
                self.err(
                    "settings.header",
                    h.pos,
                    "Room files (`room settings`) are checked by the lobby, which is not built yet. This tool checks player and preset files.".into(),
                    None,
                );
                return;
            }
            Header::Preset(n) => {
                layer.preset = Some(n.clone());
                if let Some(p) = &f.player {
                    self.err(
                        "settings.header",
                        p.pos,
                        "A preset names no player.".into(),
                        Some("remove the `player` line".into()),
                    );
                }
            }
            Header::Player => {}
        }
        if let Some(n) = &f.world.name
            && n.v != self.s.world
        {
            let msg =
                format!("This file is for '{}', and the world found is '{}'.", n.v, self.s.world);
            self.err("settings.world_not_found", n.pos, msg, None);
        }
        if let Some(req) = &f.world.version {
            match semver::VersionReq::parse(&req.v) {
                Err(e) => {
                    let msg = format!("`{}` is not a version requirement: {e}.", req.v);
                    self.err(
                        "settings.game_version",
                        req.pos,
                        msg,
                        Some("write one such as \"0.1\" or \"=0.1.3\"".into()),
                    );
                }
                Ok(r) => {
                    let ok = semver::Version::parse(&self.s.version).is_ok_and(|v| r.matches(&v));
                    if !ok {
                        let msg = format!(
                            "This file is for {} {}. The world found is {}.",
                            self.s.title(),
                            req.v,
                            self.s.version
                        );
                        let help = format!(
                            "use {} {}, or remove `version \"{}\"` to accept any version",
                            self.s.title(),
                            req.v,
                            req.v
                        );
                        self.err("settings.game_version", req.pos, msg, Some(help));
                    }
                }
            }
        }
        if let Some(p) = &f.player {
            let (chars, bytes) = (p.v.chars().count(), p.v.len());
            let l = self.l;
            if chars > l.settings_player_max_chars || bytes > l.settings_player_max_bytes {
                let msg = format!(
                    "A player name is at most {} characters and {} bytes (settings_player_max_chars, settings_player_max_bytes).",
                    l.settings_player_max_chars, l.settings_player_max_bytes
                );
                self.err("settings.limit", p.pos, msg, None);
            } else if p.v.chars().any(|c| c.is_control() || is_format_char(c)) {
                self.err(
                    "settings.limit",
                    p.pos,
                    "A player name holds no control or format characters.".into(),
                    None,
                );
            } else {
                layer.player = Some(nfc(&p.v));
            }
        }
    }

    /// The option `key` names, or the error for a key a player file may not set.
    fn option(&mut self, key: &Node<String>) -> Option<&Opt> {
        let s = self.s;
        let Some(opt) = s.options.get(&key.v) else {
            let alias = s.options.values().find(|o| o.aliases.contains(&key.v.as_str()));
            let help = match alias {
                Some(o) => Some(format!(
                    "did you mean `{}` ({}, also called {})?",
                    o.key,
                    self.label(o),
                    key.v
                )),
                None => suggest::nearest(&key.v, s.order.iter().map(String::as_str)).map(|n| {
                    let label = s.options[n]
                        .label
                        .as_ref()
                        .map_or(String::new(), |l| format!(" (\"{l}\")"));
                    format!("did you mean `{n}`{label}?")
                }),
            };
            let msg = format!("{} has no setting called '{}'.", s.title(), key.v);
            self.err("settings.unknown_option", key.pos, msg, help);
            return None;
        };
        if opt.kind == Kind::Room {
            let msg = format!("The room host decides '{}'.", self.label(opt));
            self.err(
                "settings.room_decides",
                key.pos,
                msg,
                Some("remove this line; the room's value is used".into()),
            );
            return None;
        }
        if opt.needs_offer && !s.offers.contains(&opt.key) {
            let msg = format!("{} does not offer {}.", s.title(), self.label(opt).to_lowercase());
            self.err(
                "settings.not_offered",
                key.pos,
                msg,
                Some(format!("remove this line; the game cannot use '{}'", opt.key)),
            );
            return None;
        }
        Some(opt)
    }

    fn line(&mut self, line: &Line, layer: &mut Layer) {
        match &line.stmt {
            Stmt::Setting { key, value, .. } => {
                let Some(opt) = self.option(key).cloned() else { return };
                let Some(val) = self.value(&opt, value) else { return };
                if let Some(first) = layer.values.get(&key.v) {
                    let msg = format!(
                        "'{}' is set twice, on lines {} and {}. Keep one.",
                        self.label(&opt),
                        first.pos.line,
                        line.pos.line
                    );
                    self.err("settings.set_twice", line.pos, msg, None);
                    return;
                }
                layer.values.insert(key.v.clone(), Setting { val, pos: line.pos });
            }
            Stmt::Block { when, chance, body, .. } => {
                self.block(line, when, chance.clone(), body, layer)
            }
            Stmt::Place { items, places, mods } => {
                if let Some(p) = self.place(line.pos, items, places, mods) {
                    layer.place.push(p);
                }
            }
            Stmt::Connect { from, to, dir, chance } => {
                let mut doors = Vec::from([from.v.clone()]);
                let target = match &to.v {
                    V::OneOf(_) => {
                        // The same checks as any one_of: distinct doors, at least 2.
                        let mut door = |c: &mut Self, e: &Node<V>| -> Option<Value> {
                            let V::Name(n) = &e.v else {
                                let msg =
                                    format!("`connect` draws among doors, not {}.", shape(&e.v));
                                c.err("settings.surprise_shape", e.pos, msg, None);
                                return None;
                            };
                            Some(json!(n))
                        };
                        let Some(spec) = self.draw("This connection", to, &mut door) else {
                            return;
                        };
                        doors.extend(
                            spec["one_of"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|d| d.as_str().map(ToOwned::to_owned)),
                        );
                        spec
                    }
                    V::Name(n) => {
                        doors.push(n.clone());
                        json!(n)
                    }
                    _ => return,
                };
                let mut ok = true;
                for (d, pos) in
                    doors.iter().zip(core::iter::once(from.pos).chain(core::iter::repeat(to.pos)))
                {
                    if !self.s.doors.contains(d) {
                        let help = suggest::nearest(d, self.s.doors.iter().map(String::as_str))
                            .map(|n| format!("did you mean `{n}`?"));
                        let msg = format!("'{d}' is not a door in {}.", self.s.title());
                        self.err("settings.unknown_name", pos, msg, help);
                        ok = false;
                    }
                }
                if ok {
                    let dir = match dir {
                        Dir::Both => "both",
                        Dir::OneWay => "one_way",
                        Dir::WayOut => "way_out",
                    };
                    if let Some(c) = chance {
                        self.percent(c.v, c.pos);
                    }
                    let chance = chance.as_ref().map_or(100, |c| c.v);
                    layer.connect.push((
                        json!({"from": from.v, "to": target, "dir": dir, "chance": chance}),
                        doors,
                        line.pos,
                    ));
                }
            }
            Stmt::Pin { thing, slot, shuffle } => {
                let Some(sh) = self.s.shuffles.get(&shuffle.v) else {
                    let help =
                        suggest::nearest(&shuffle.v, self.s.shuffles.keys().map(String::as_str))
                            .map(|n| format!("did you mean `{n}`?"));
                    let msg = format!("'{}' is not a shuffle in {}.", shuffle.v, self.s.title());
                    self.err("settings.unknown_name", shuffle.pos, msg, help);
                    return;
                };
                let mut ok = true;
                for (n, set, what) in [(thing, &sh.things, "thing"), (slot, &sh.slots, "slot")] {
                    if !set.contains(&n.v) {
                        let msg =
                            format!("'{}' is not a {what} of the shuffle '{}'.", n.v, shuffle.v);
                        let help = suggest::nearest(&n.v, set.iter().map(String::as_str))
                            .map(|x| format!("did you mean `{x}`?"));
                        self.err("settings.placement", n.pos, msg, help);
                        ok = false;
                    }
                }
                if ok {
                    layer.pin.push((
                        json!({"thing": thing.v, "slot": slot.v, "shuffle": shuffle.v}),
                        slot.v.clone(),
                        line.pos,
                    ));
                }
            }
            Stmt::Legacy { body, .. } => {
                for l in body {
                    if let Stmt::Setting { key, value, .. } = &l.stmt {
                        match legacy(&value.v) {
                            Some(_) if layer.legacy.contains_key(&key.v) => {
                                let msg = format!(
                                    "The legacy key '{}' is set twice in this file.",
                                    key.v
                                );
                                self.err(
                                    "settings.set_twice",
                                    key.pos,
                                    msg,
                                    Some("keep one".into()),
                                );
                            }
                            Some(v) => {
                                layer.legacy.insert(key.v.clone(), v);
                            }
                            None => self.err(
                                "settings.bad_value",
                                value.pos,
                                "A `legacy` line holds a plain value.".into(),
                                None,
                            ),
                        }
                    }
                }
            }
        }
    }

    fn percent(&mut self, n: i64, pos: Pos) {
        if !(1..=100).contains(&n) {
            self.err(
                "settings.chances_total",
                pos,
                format!("A chance is 1% to 100%, not {n}%."),
                None,
            );
        }
    }

    fn block(
        &mut self,
        line: &Line,
        when: &[(Node<String>, Node<V>)],
        chance: Option<Node<i64>>,
        body: &[Line],
        layer: &mut Layer,
    ) {
        let mut b = Block {
            when: Vec::new(),
            chance: chance.as_ref().map(|c| c.v),
            with: when.is_empty(),
            changes: Vec::new(),
            cosmetic: false,
            pos: line.pos,
        };
        if let Some(c) = &chance {
            self.percent(c.v, c.pos);
        }
        let mut scopes = BTreeSet::new();
        let mut ok = true;
        let mut seen = BTreeSet::new();
        for (key, value) in when {
            if !seen.insert(key.v.as_str()) {
                let msg = format!("This block tests '{}' twice.", key.v);
                self.err("settings.block", key.pos, msg, Some("test each option once".into()));
                ok = false;
                continue;
            }
            let Some(opt) = self.option(key).cloned() else {
                ok = false;
                continue;
            };
            scopes.insert(opt.scope == Scope::Cosmetic);
            match self.plain(&opt, value, false) {
                Some(v) => b.when.push((key.v.clone(), v)),
                None => ok = false,
            }
        }
        let tested: BTreeSet<&str> = when.iter().map(|w| w.0.v.as_str()).collect();
        for l in body {
            let Stmt::Setting { key, op, value } = &l.stmt else { continue };
            let Some(opt) = self.option(key).cloned() else {
                ok = false;
                continue;
            };
            scopes.insert(opt.scope == Scope::Cosmetic);
            if tested.contains(key.v.as_str()) {
                let msg = format!("This block tests '{}' and changes it too.", self.label(&opt));
                self.err(
                    "settings.block_self",
                    key.pos,
                    msg,
                    Some("test it in this block, or change it, not both".into()),
                );
                ok = false;
                continue;
            }
            if b.changes.iter().any(|c| c.key == key.v) {
                let msg = format!("This block changes '{}' twice.", self.label(&opt));
                self.err("settings.block", key.pos, msg, Some("keep one change".into()));
                ok = false;
                continue;
            }
            let val = match op {
                Op::Set => self.value(&opt, value),
                Op::Add | Op::Remove => self.delta(&opt, value, key.pos).map(Val::Plain),
            };
            match val {
                Some(val) => {
                    b.changes.push(Change { key: key.v.clone(), op: *op, val, pos: l.pos })
                }
                None => ok = false,
            }
        }
        if scopes.len() > 1 {
            self.err(
                "settings.block",
                line.pos,
                "This block mixes looks-only options with options that change the game.".into(),
                Some("split it into two blocks".into()),
            );
            return;
        }
        b.cosmetic = scopes.contains(&true);
        if !ok {
            return;
        }
        let same = |x: &Block| {
            x.when == b.when
                && x.chance == b.chance
                && x.changes
                    .iter()
                    .map(|c| (&c.key, c.op, &c.val))
                    .eq(b.changes.iter().map(|c| (&c.key, c.op, &c.val)))
        };
        if let Some(first) = layer.blocks.iter().find(|x| same(x)) {
            let msg = format!("This block is the same as the one on line {}.", first.pos.line);
            self.err("settings.rule_duplicate", line.pos, msg, Some("remove one".into()));
            return;
        }
        layer.blocks.push(b);
    }

    /// The names or counts `+=` or `-=` adds or removes: list and counts kinds only.
    fn delta(&mut self, opt: &Opt, v: &Node<V>, at: Pos) -> Option<Value> {
        match (&opt.kind, &v.v) {
            (Kind::Set { of, copies, .. }, V::List(entries)) => {
                self.list(opt, of, *copies, None, entries, v.pos)
            }
            (Kind::Counts { of, .. }, V::Record(fields)) => {
                let mut out = Map::new();
                for (k, n) in fields {
                    if out.contains_key(&k.v) {
                        let msg = format!("'{}' is written twice.", k.v);
                        self.err("settings.bad_value", k.pos, msg, Some("keep one".into()));
                        return None;
                    }
                    if !of.contains(&k.v) {
                        self.unknown_word(opt, of, &k.v, k.pos);
                        return None;
                    }
                    let V::Int(n) = n.v else {
                        self.err(
                            "settings.bad_value",
                            n.pos,
                            format!("'{}' counts in whole numbers.", self.label(opt)),
                            None,
                        );
                        return None;
                    };
                    out.insert(k.v.clone(), json!(n));
                }
                Some(Value::Object(out))
            }
            (Kind::Set { .. } | Kind::Counts { .. }, _) => {
                let msg = format!(
                    "`+=` and `-=` on '{}' take {}, not {}.",
                    self.label(opt),
                    if opt.kind.is_list() { "a list" } else { "a record of counts" },
                    shape(&v.v)
                );
                self.err("settings.bad_value", v.pos, msg, None);
                None
            }
            _ => {
                let msg = format!(
                    "`+=` and `-=` change lists and counts; '{}' is neither. Write `=` with the new value.",
                    self.label(opt)
                );
                self.err("settings.block", at, msg, None);
                None
            }
        }
    }

    /// A value of `opt`: plain or a surprise.
    fn value(&mut self, opt: &Opt, v: &Node<V>) -> Option<Val> {
        if v.v.is_surprise() {
            self.surprise(opt, v).map(Val::Surprise)
        } else {
            self.plain(opt, v, true).map(Val::Plain)
        }
    }

    fn bad(&mut self, opt: &Opt, v: &Node<V>, want: &str) {
        let msg = format!("'{}' takes {want}. Your file says {}.", self.label(opt), shape(&v.v));
        self.err("settings.bad_value", v.pos, msg, None);
    }

    /// A plain value in canonical form. `limits`: hold a list to the option's own count
    /// limits (not for a condition).
    fn plain(&mut self, opt: &Opt, v: &Node<V>, limits: bool) -> Option<Value> {
        let label = self.label(opt);
        match (&opt.kind, &v.v) {
            (Kind::Toggle, V::Name(n)) => match n.as_str() {
                "on" | "true" => Some(json!(true)),
                "off" | "false" => Some(json!(false)),
                _ => {
                    let msg = format!("'{label}' is on or off. Your file says '{n}'.");
                    self.err("settings.bad_value", v.pos, msg, Some("write on or off".into()));
                    None
                }
            },
            (Kind::Choice { values, .. }, V::Name(n)) => {
                if values.contains(n) {
                    return Some(json!(n));
                }
                let flip = match n.as_str() {
                    "true" => "on",
                    "false" => "off",
                    _ => "",
                };
                if !flip.is_empty() && values.iter().any(|x| x == flip) {
                    return Some(json!(flip));
                }
                let msg = format!("'{label}' can be {}. Your file says '{n}'.", or_list(values));
                let help = suggest::help(
                    n,
                    values.iter().map(String::as_str),
                    &format!("write one of: {}", values.join(", ")),
                );
                self.err("settings.bad_value", v.pos, msg, Some(help));
                None
            }
            (Kind::Choice { or_text: Some(max), .. } | Kind::Text { max_len: max }, V::Str(s)) => {
                let text = nfc(s);
                if text.len() > *max {
                    let msg = format!("'{label}' is at most {max} bytes long.");
                    self.err("settings.out_of_range", v.pos, msg, None);
                    return None;
                }
                Some(if matches!(opt.kind, Kind::Choice { .. }) {
                    json!({"text": text})
                } else {
                    json!(text)
                })
            }
            (Kind::Range { min, max, named }, V::Int(n)) => {
                if (*min..=*max).contains(n) {
                    Some(json!(n))
                } else {
                    let msg = format!("'{label}' is {min} to {max}. Your file says {n}.");
                    self.err("settings.out_of_range", v.pos, msg, None);
                    let _ = named;
                    None
                }
            }
            (Kind::Range { min, max, named }, V::Name(n)) => {
                if let Some(x) = named.get(n) {
                    return Some(json!(x));
                }
                let msg =
                    format!("'{label}' is a number from {min} to {max}. Your file says '{n}'.");
                let help = suggest::help(n, named.keys().map(String::as_str), "");
                self.err("settings.bad_value", v.pos, msg, (!help.is_empty()).then_some(help));
                None
            }
            (Kind::Set { of, count, copies }, V::List(entries)) => {
                let count = if limits { *count } else { None };
                self.list(opt, of, *copies, count, entries, v.pos)
            }
            (Kind::Counts { of, min, max }, V::Record(fields)) => {
                let mut out = opt.default.as_object().cloned().unwrap_or_default();
                let mut seen = BTreeSet::new();
                for (k, n) in fields {
                    if !of.contains(&k.v) {
                        self.unknown_word(opt, of, &k.v, k.pos);
                        return None;
                    }
                    if !seen.insert(k.v.as_str()) {
                        self.err(
                            "settings.bad_value",
                            k.pos,
                            format!("'{}' is written twice.", k.v),
                            None,
                        );
                        return None;
                    }
                    match n.v {
                        V::Int(x) if (*min..=*max).contains(&x) => {
                            out.insert(k.v.clone(), json!(x));
                        }
                        V::Int(x) => {
                            let msg = format!(
                                "Each count of '{label}' is {min} to {max}. Your file says {x}."
                            );
                            self.err("settings.out_of_range", n.pos, msg, None);
                            return None;
                        }
                        _ => {
                            self.err(
                                "settings.bad_value",
                                n.pos,
                                format!("'{label}' counts in whole numbers."),
                                None,
                            );
                            return None;
                        }
                    }
                }
                Some(Value::Object(out))
            }
            (Kind::Record(kinds), V::Record(fields)) => {
                let mut out = opt.default.as_object().cloned().unwrap_or_default();
                let mut seen = BTreeSet::new();
                for (k, fv) in fields {
                    if !seen.insert(k.v.as_str()) {
                        let msg = format!("'{label}' has its field '{}' twice.", k.v);
                        self.err("settings.bad_value", k.pos, msg, Some("keep one".into()));
                        return None;
                    }
                    let Some((_, kind)) = kinds.iter().find(|(n, _)| *n == k.v) else {
                        let names: Vec<&str> = kinds.iter().map(|(n, _)| n.as_str()).collect();
                        let msg = format!("'{label}' has no field '{}'.", k.v);
                        let help = suggest::help(
                            &k.v,
                            names.iter().copied(),
                            &format!("its fields are {}", names.join(", ")),
                        );
                        self.err("settings.bad_value", k.pos, msg, Some(help));
                        return None;
                    };
                    let field = Opt {
                        key: format!("{}.{}", opt.key, k.v),
                        label: Some(format!("{label}: {}", k.v)),
                        kind: kind.clone(),
                        default: Value::Null,
                        ..opt.clone()
                    };
                    let x = self.plain(&field, fv, limits)?;
                    out.insert(k.v.clone(), x);
                }
                if opt.key == "start_with" {
                    self.check_from_shuffle(&out, v.pos);
                }
                Some(Value::Object(out))
            }
            (kind, _) => {
                let want = match kind {
                    Kind::Toggle => "on or off",
                    Kind::Range { .. } => "a whole number",
                    Kind::Choice { or_text: Some(_), .. } => "one of its values or a string",
                    Kind::Choice { .. } => "one of its values",
                    Kind::Text { .. } => "a string",
                    Kind::Set { .. } => "a list, like [a, b]",
                    Kind::Counts { .. } => "a count per name, like { a = 3 }",
                    Kind::Record(_) => "a record, like { field = value }",
                    Kind::Room => "nothing in this file",
                };
                self.bad(opt, v, want);
                None
            }
        }
    }

    /// `start_with.from_shuffle` takes at most the copies the shuffle holds.
    fn check_from_shuffle(&mut self, rec: &Map<String, Value>, pos: Pos) {
        for pair in rec.get("from_shuffle").and_then(Value::as_array).into_iter().flatten() {
            let (Some(name), Some(n)) = (pair[0].as_str(), pair[1].as_u64()) else { continue };
            if let Some(have) = self.s.items.get(name).and_then(|i| i.copies)
                && n > have
            {
                let msg =
                    format!("The shuffle holds {have} of '{name}', and `from_shuffle` takes {n}.");
                self.err("settings.placement", pos, msg, None);
            }
        }
    }

    fn unknown_word(&mut self, opt: &Opt, of: &[String], n: &str, pos: Pos) {
        let msg = format!("'{}' takes {}. Your file says '{n}'.", self.label(opt), or_list(of));
        let help = suggest::help(n, of.iter().map(String::as_str), "");
        self.err("settings.bad_value", pos, msg, (!help.is_empty()).then_some(help));
    }

    fn count_expanded(&mut self, n: usize, pos: Pos) -> bool {
        self.expanded += n;
        if self.expanded > self.l.settings_max_expanded_names {
            if self.expanded - n <= self.l.settings_max_expanded_names {
                let msg = format!(
                    "This file reaches more than {} names once groups are expanded (settings_max_expanded_names).",
                    self.l.settings_max_expanded_names
                );
                self.err("settings.limit", pos, msg, None);
            }
            return false;
        }
        true
    }

    /// What `name` stands for in a list of `of`: itself, or a group's members. `place`: the
    /// per-seed groups are allowed (a `place` position).
    fn expand(&mut self, of: &Of, name: &str, pos: Pos, place: bool) -> Option<Vec<String>> {
        let s = self.s;
        if self.expanded > self.l.settings_max_expanded_names {
            // The cap is passed: nothing more is expanded in this file.
            return None;
        }
        // The size first, so a group past the cap is refused before it is copied.
        let size = match of {
            Of::Links => Some(1),
            Of::Words(w) => w.iter().any(|x| x == name).then_some(1),
            Of::Items if s.items.contains_key(name) => Some(1),
            Of::Items if name == EVERY_ITEM => Some(s.items.len()),
            Of::Items => s.item_groups.get(name).map(Vec::len),
            Of::Places if s.places.contains(name) => Some(1),
            Of::Places if name == EVERY_PLACE => Some(s.places.len()),
            Of::Places if name == EARLY_PLACES || name == LATER_PLACES => {
                if !place {
                    let msg = format!(
                        "`{name}` is known only once the seed is made, so it may stand only after `place ... at`."
                    );
                    self.err("settings.bad_value", pos, msg, None);
                    return None;
                }
                Some(0)
            }
            Of::Places => s.place_groups.get(name).map(Vec::len),
        };
        if let Some(n) = size {
            if !self.count_expanded(n, pos) {
                return None;
            }
            return Some(match of {
                Of::Items if name == EVERY_ITEM => s.items.keys().cloned().collect(),
                Of::Items if !s.items.contains_key(name) => s.item_groups[name].clone(),
                Of::Places if name == EVERY_PLACE => s.places.iter().cloned().collect(),
                Of::Places if name == EARLY_PLACES || name == LATER_PLACES => Vec::new(),
                Of::Places if !s.places.contains(name) => s.place_groups[name].clone(),
                _ => Vec::from([name.to_owned()]),
            });
        }
        let (what, pool): (&str, Vec<&str>) = match of {
            Of::Items => (
                "an item",
                s.items.keys().chain(s.item_groups.keys()).map(String::as_str).collect(),
            ),
            Of::Places => (
                "a place",
                s.places.iter().chain(s.place_groups.keys()).map(String::as_str).collect(),
            ),
            Of::Words(w) => ("a word this option takes", w.iter().map(String::as_str).collect()),
            Of::Links => unreachable!(),
        };
        let help = suggest::nearest(name, pool).map(|n| format!("did you mean '{n}'?"));
        let key =
            if matches!(of, Of::Words(_)) { "settings.bad_value" } else { "settings.unknown_name" };
        let msg = format!("'{name}' is not {what} in {}.", s.title());
        self.err(key, pos, msg, help);
        None
    }

    /// A list of `of`, sorted, groups expanded: names, or `[name, count]` pairs.
    fn list(
        &mut self,
        opt: &Opt,
        of: &Of,
        copies: bool,
        count: Option<(u64, u64)>,
        entries: &[Node<Entry>],
        pos: Pos,
    ) -> Option<Value> {
        let mut out: BTreeMap<String, i64> = BTreeMap::new();
        let mut written = BTreeSet::new();
        let mut ok = true;
        for e in entries {
            let name = &e.v.name;
            if !written.insert(name.v.as_str()) {
                self.err(
                    "settings.bad_value",
                    name.pos,
                    format!("'{}' is written twice in this list.", name.v),
                    Some("keep one".into()),
                );
                ok = false;
                continue;
            }
            let n = match e.v.count.as_ref().map(|c| c.v) {
                None => 1,
                Some(Count::All) => {
                    self.err(
                        "settings.placement",
                        e.pos,
                        "`x all` is only for `place`.".into(),
                        None,
                    );
                    ok = false;
                    continue;
                }
                Some(Count::N(_)) if !copies => {
                    self.err(
                        "settings.bad_value",
                        e.pos,
                        format!("'{}' takes names without counts.", self.label(opt)),
                        None,
                    );
                    ok = false;
                    continue;
                }
                Some(Count::N(n)) => n,
            };
            let Some(members) = self.expand(of, &name.v, name.pos, false) else {
                ok = false;
                continue;
            };
            if copies && !self.add_copies(n.unsigned_abs() * members.len() as u64, e.pos) {
                ok = false;
            }
            for m in members {
                *out.entry(m).or_default() += n;
            }
        }
        if !ok {
            return None;
        }
        if let Some((lo, hi)) = count {
            let len = out.len() as u64;
            if len < lo || len > hi {
                let msg = format!(
                    "'{}' takes {lo} to {hi} entries, and your list has {len}.",
                    self.label(opt)
                );
                self.err("settings.out_of_range", pos, msg, None);
                return None;
            }
        }
        Some(if copies {
            out.into_iter().map(|(k, n)| json!([k, n])).collect()
        } else {
            out.into_keys().map(Value::String).collect()
        })
    }

    fn add_copies(&mut self, n: u64, pos: Pos) -> bool {
        self.copies = self.copies.saturating_add(n);
        if self.copies > self.l.settings_max_total_copies {
            if self.copies - n <= self.l.settings_max_total_copies {
                let msg = format!(
                    "This file names more than {} item copies (settings_max_total_copies).",
                    self.l.settings_max_total_copies
                );
                self.err("settings.limit", pos, msg, None);
            }
            return false;
        }
        true
    }

    /// A surprise of `opt`, as its spec (doc 20 sections 2.10 and 6).
    fn surprise(&mut self, opt: &Opt, v: &Node<V>) -> Option<Value> {
        let label = self.label(opt);
        let no_draw = matches!(opt.kind, Kind::Text { .. } | Kind::Counts { .. } | Kind::Record(_));
        match &v.v {
            V::Any if no_draw => {
                let msg =
                    format!("'{label}' has no list of values to draw from, so it cannot be `any`.");
                self.err("settings.surprise_shape", v.pos, msg, None);
                None
            }
            V::Any => Some(json!({"any": {}})),
            V::Between(a, b, lean) => self.between(opt, v.pos, a, b, lean.as_ref()),
            V::SomeOf(size, names) => self.some_of(opt, v.pos, size, names),
            V::OneOf(_) | V::Chance(_)
                if opt.kind.is_list()
                    || matches!(opt.kind, Kind::Counts { .. } | Kind::Record(_)) =>
            {
                let msg =
                    format!("'{label}' is a list or record; `one_of` and `chance` pick one value.");
                let help = opt
                    .kind
                    .is_list()
                    .then(|| "use some_of(N, names...) to draw some of the names".to_owned());
                self.err("settings.surprise_shape", v.pos, msg, help);
                None
            }
            V::OneOf(_) | V::Chance(_) => {
                self.draw(&label, v, &mut |c: &mut Self, e: &Node<V>| c.choice(opt, e))
            }
            _ => None,
        }
    }

    /// A `one_of` or `chance`, each entry checked by `entry`: shared by option values and
    /// `place`, so both refuse a value written twice, a chance outside 1% to 100% and a total
    /// other than 100. `one_of` is sorted (equal odds); `chance` keeps its written order, which
    /// the draw follows (2.10).
    fn draw(
        &mut self,
        label: &str,
        v: &Node<V>,
        entry: &mut dyn FnMut(&mut Self, &Node<V>) -> Option<Value>,
    ) -> Option<Value> {
        let entries: Vec<(&Node<V>, Option<&Node<i64>>)> = match &v.v {
            V::OneOf(list) => list.iter().map(|e| (e, None)).collect(),
            V::Chance(list) => list.iter().map(|(e, p)| (e, Some(p))).collect(),
            _ => return None,
        };
        let mut out: Vec<(Value, Option<i64>)> = Vec::new();
        let mut total = 0i64;
        for (e, p) in entries {
            let x = entry(self, e)?;
            if out.iter().any(|o| o.0 == x) {
                let help = Some("keep one".into());
                self.err(
                    "settings.surprise_shape",
                    e.pos,
                    "This value is written twice.".into(),
                    help,
                );
                return None;
            }
            if let Some(p) = p {
                if !(1..=100).contains(&p.v) {
                    let msg = format!("A chance is 1% to 100%, not {}%.", p.v);
                    self.err("settings.chances_total", p.pos, msg, None);
                    return None;
                }
                total += p.v;
            }
            out.push((x, p.map(|p| p.v)));
        }
        if matches!(v.v, V::OneOf(_)) {
            if out.len() < 2 {
                let msg = "one_of needs at least 2 different values.".into();
                self.err("settings.surprise_shape", v.pos, msg, None);
                return None;
            }
            let mut vals: Vec<Value> = out.into_iter().map(|o| o.0).collect();
            vals.sort_by_key(spec_key);
            return Some(json!({"one_of": vals}));
        }
        if total != 100 {
            let more = if total < 100 {
                format!("{} more", 100 - total)
            } else {
                format!("{} less", total - 100)
            };
            let msg = format!(
                "The chances for '{label}' add up to {total}. They need to add up to 100 ({more})."
            );
            self.err("settings.chances_total", v.pos, msg, None);
            return None;
        }
        Some(json!({"chance": out.into_iter().map(|(x, p)| json!([x, p])).collect::<Vec<_>>()}))
    }

    /// One entry of `one_of` or `chance`: a plain value, `any` or `between`, one level deep.
    fn choice(&mut self, opt: &Opt, e: &Node<V>) -> Option<Value> {
        match &e.v {
            V::Any | V::Between(..) => self.surprise(opt, e),
            V::OneOf(_) | V::Chance(_) | V::SomeOf(..) => {
                self.err("settings.surprise_shape", e.pos, "A surprise inside one_of or chance is only `any` or `between`: one level deep.".into(), None);
                None
            }
            V::Name(_) | V::Int(_) | V::Str(_) => self.plain(opt, e, true),
            _ => {
                self.bad(opt, e, "one value per entry");
                None
            }
        }
    }

    fn between(
        &mut self,
        opt: &Opt,
        pos: Pos,
        a: &Node<i64>,
        b: &Node<i64>,
        lean: Option<&Node<String>>,
    ) -> Option<Value> {
        let label = self.label(opt);
        let Kind::Range { min, max, .. } = opt.kind else {
            let msg = format!("between(...) draws a number; '{label}' is not a number option.");
            self.err("settings.surprise_shape", pos, msg, None);
            return None;
        };
        if a.v >= b.v {
            let msg = format!("between({}, {}) needs its first number below its second.", a.v, b.v);
            self.err(
                "settings.surprise_shape",
                pos,
                msg,
                Some("for one number, write the number".into()),
            );
            return None;
        }
        for n in [a, b] {
            if !(min..=max).contains(&n.v) {
                let msg = format!("'{label}' is {min} to {max}, and between reaches {}.", n.v);
                self.err("settings.out_of_range", n.pos, msg, None);
                return None;
            }
        }
        match lean.map(|l| l.v.as_str()) {
            None => Some(json!({"between": [a.v, b.v]})),
            Some(w @ ("low" | "middle" | "high")) => Some(json!({"between": [a.v, b.v, w]})),
            Some(w) => {
                let l = lean.map_or(pos, |l| l.pos);
                let help = suggest::help(w, ["high", "low", "middle"], "write low, middle or high");
                self.err(
                    "settings.surprise_shape",
                    l,
                    format!("between leans `low`, `middle` or `high`, not `{w}`."),
                    Some(help),
                );
                None
            }
        }
    }

    fn some_of(
        &mut self,
        opt: &Opt,
        pos: Pos,
        size: &Node<Size>,
        names: &[Node<String>],
    ) -> Option<Value> {
        let label = self.label(opt);
        let Kind::Set { of, count, copies: false } = &opt.kind else {
            let msg = format!("some_of(...) draws part of a list; '{label}' is not a list option.");
            self.err("settings.surprise_shape", pos, msg, None);
            return None;
        };
        let pool = self.pool(of, names)?;
        let (lo, hi) = count.unwrap_or((0, u64::MAX));
        let n = pool.len() as i64;
        let (a, b) = match size.v {
            Size::N(x) => (x, x),
            Size::Range(a, b) => {
                if a >= b {
                    self.err(
                        "settings.surprise_shape",
                        size.pos,
                        format!("some_of({a} to {b}, ...) runs low to high."),
                        Some(format!("for one size, write {a}")),
                    );
                    return None;
                }
                (a, b)
            }
            Size::Any => {
                let top = hi.min(pool.len() as u64);
                if lo > top {
                    let msg = format!(
                        "'{label}' takes {lo} to {hi} entries, and some_of has only {} to pick from.",
                        pool.len()
                    );
                    self.err("settings.out_of_range", size.pos, msg, None);
                    return None;
                }
                let names: Vec<&str> = names.iter().map(|x| x.v.as_str()).collect();
                return Some(json!({"some_of": {"size": "any", "names": sorted(&names)}}));
            }
        };
        if a < 0 || b > n {
            let msg = format!(
                "some_of can pick 0 to {n} of these {n} names, not {}.",
                if a < 0 { a } else { b }
            );
            self.err("settings.surprise_shape", size.pos, msg, None);
            return None;
        }
        if (a as u64) < lo || (b as u64) > hi {
            let msg = format!(
                "'{label}' takes {lo} to {hi} entries, and some_of can pick {}.",
                if (a as u64) < lo { a } else { b }
            );
            self.err(
                "settings.out_of_range",
                size.pos,
                msg,
                Some(format!("write some_of({lo} to {}, ...) or a smaller list", hi.min(n as u64))),
            );
            return None;
        }
        let names: Vec<&str> = names.iter().map(|x| x.v.as_str()).collect();
        Some(json!({"some_of": {"size": [a, b], "names": sorted(&names)}}))
    }

    /// The names a `some_of` draws from, groups expanded.
    fn pool(&mut self, of: &Of, names: &[Node<String>]) -> Option<BTreeSet<String>> {
        let mut pool = BTreeSet::new();
        let mut written = BTreeSet::new();
        for n in names {
            if !written.insert(n.v.as_str()) {
                self.err(
                    "settings.surprise_shape",
                    n.pos,
                    format!("'{}' is written twice.", n.v),
                    Some("keep one".into()),
                );
                return None;
            }
            pool.extend(self.expand(of, &n.v, n.pos, false)?);
        }
        if pool.is_empty() {
            let at = names.first().map_or(Pos::default(), |n| n.pos);
            let msg = "some_of draws from these names, and they hold none.".into();
            self.err("settings.surprise_shape", at, msg, None);
            return None;
        }
        Some(pool)
    }

    fn place(
        &mut self,
        pos: Pos,
        items: &Node<Items>,
        places: &Node<V>,
        mods: &[Node<Mod>],
    ) -> Option<Placement> {
        let copy = mods.iter().any(|m| m.v == Mod::Copy);
        let if_possible = mods.iter().any(|m| m.v == Mod::IfPossible);
        let chance = mods
            .iter()
            .find_map(|m| if let Mod::Chance(c) = m.v { Some((c, m.pos)) } else { None });
        if let Some((c, at)) = chance {
            self.percent(c, at);
        }
        let mut named: Vec<(String, Option<i64>)> = Vec::new();
        let mut ok = true;
        let mut entry = |c: &mut Self, e: &Entry, at: Pos| -> Option<Value> {
            let members = c.expand(&Of::Items, &e.name.v, e.name.pos, false)?;
            let n = match e.count.as_ref().map(|x| x.v) {
                None => Some(1),
                Some(Count::N(n)) => Some(n),
                Some(Count::All) => None,
            };
            if n.is_none() && copy {
                c.err(
                    "settings.placement",
                    at,
                    "`x all` takes copies from the shuffle; it cannot be a `copy`.".into(),
                    None,
                );
                return None;
            }
            if let Some(n) = n
                && !c.add_copies(n.unsigned_abs() * members.len() as u64, at)
            {
                return None;
            }
            named.extend(members.into_iter().map(|m| (m, n)));
            Some(json!([e.name.v, n.map_or(json!("all"), |n| json!(n))]))
        };
        let (items_json, drawn) = match &items.v {
            Items::One(e) => (json!([entry(self, e, items.pos)?]), 0),
            Items::List(list) => {
                let mut out = Vec::new();
                for e in list {
                    match entry(self, &e.v, e.pos) {
                        Some(j) => out.push(j),
                        None => ok = false,
                    }
                }
                out.sort_by_key(spec_key);
                (Value::Array(out), 0)
            }
            Items::Pick(V::SomeOf(size, names)) => {
                let pool = self.pool(&Of::Items, names)?;
                let (a, b) = match size.v {
                    Size::N(x) => (x, x),
                    Size::Range(a, b) => (a, b),
                    Size::Any => (0, pool.len() as i64),
                };
                if a < 0
                    || a > b
                    || b > pool.len() as i64
                    || (matches!(size.v, Size::Range(..)) && a == b)
                {
                    self.err(
                        "settings.surprise_shape",
                        size.pos,
                        format!(
                            "some_of here can pick 0 to {} items, from low to high.",
                            pool.len()
                        ),
                        None,
                    );
                    return None;
                }
                let names: Vec<&str> = names.iter().map(|x| x.v.as_str()).collect();
                let size = if matches!(size.v, Size::Any) { json!("any") } else { json!([a, b]) };
                (json!({"some_of": {"size": size, "names": sorted(&names)}}), b)
            }
            Items::Pick(pick @ (V::OneOf(_) | V::Chance(_))) => {
                let node = Node { v: pick.clone(), pos: items.pos, span: items.span };
                let mut item = |c: &mut Self, e: &Node<V>| -> Option<Value> {
                    let V::Name(n) = &e.v else {
                        let msg = "`place` draws among items.".into();
                        c.err("settings.surprise_shape", e.pos, msg, None);
                        return None;
                    };
                    c.expand(&Of::Items, n, e.pos, false)?;
                    Some(json!(n))
                };
                (self.draw("This placement", &node, &mut item)?, 1)
            }
            Items::Pick(_) => return None,
        };
        // The candidate places; `None` when per-seed groups make them unknown until generation.
        let mut written: Vec<String> = Vec::new();
        let mut known: Option<BTreeSet<String>> = Some(BTreeSet::new());
        let list: Vec<(String, Pos)> = match &places.v {
            V::Name(n) => Vec::from([(n.clone(), places.pos)]),
            V::List(es) => {
                if let Some(e) = es.iter().find(|e| e.v.count.is_some()) {
                    let msg = "A place takes one item at a time: places have no counts.".into();
                    self.err("settings.bad_value", e.pos, msg, None);
                    return None;
                }
                es.iter().map(|e| (e.v.name.v.clone(), e.v.name.pos)).collect()
            }
            _ => Vec::new(),
        };
        for (n, at) in list {
            if n == EARLY_PLACES || n == LATER_PLACES {
                known = None;
            }
            match self.expand(&Of::Places, &n, at, true) {
                Some(m) => {
                    if let Some(k) = &mut known {
                        k.extend(m);
                    }
                }
                None => ok = false,
            }
            written.push(n);
        }
        if !ok {
            return None;
        }
        let copies: i64 =
            if drawn > 0 { drawn } else { named.iter().map(|(_, n)| n.unwrap_or(1)).sum() };
        let all = named.iter().any(|(_, n)| n.is_none());
        if let Some(k) = &known
            && !all
            && copies > k.len() as i64
        {
            let msg = format!(
                "This places {copies} copies at {} places; each copy needs a place of its own.",
                k.len()
            );
            self.err(
                "settings.placement",
                pos,
                msg,
                Some("list more places, or place fewer copies".into()),
            );
            return None;
        }
        written.sort();
        let single = known.as_ref().filter(|k| k.len() == 1).and_then(|k| k.first().cloned());
        let json = json!({
            "items": items_json,
            "places": written,
            "copy": copy,
            "if_possible": if_possible,
            "chance": chance.map_or(100, |c| c.0),
        });
        Some(Placement {
            json,
            items: if drawn > 0 { Vec::new() } else { named },
            single,
            strict: !if_possible && chance.is_none(),
            copy,
            pos,
        })
    }
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    v.sort();
    v
}

/// A `legacy` line's value as JSON: plain values only.
fn legacy(v: &V) -> Option<Value> {
    Some(match v {
        V::Name(n) => json!(n),
        V::Int(n) => json!(n),
        V::Str(s) => json!(nfc(s)),
        V::List(es) => es
            .iter()
            .map(|e| match e.v.count.as_ref().map(|c| c.v) {
                None => json!(e.v.name.v),
                Some(Count::N(n)) => json!([e.v.name.v, n]),
                Some(Count::All) => json!([e.v.name.v, "all"]),
            })
            .collect(),
        V::Record(f) => {
            let mut m = Map::new();
            for (k, x) in f {
                m.insert(k.v.clone(), legacy(&x.v)?);
            }
            Value::Object(m)
        }
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ast::parse;
    use crate::testdata::{CINDERFALL, LANTERNFALL};
    use alloc::string::ToString;
    use alloc::vec;

    const L: Limits = Limits::DESIGN;

    pub(crate) fn lanternfall() -> Schema {
        Schema::parse(LANTERNFALL, &L).unwrap()
    }

    fn run(src: &str) -> (Layer, Vec<Diagnostic>) {
        let (f, mut e) = parse(src, "mira-lanternfall.randoset", &L);
        let (layer, more) = check(&f, &lanternfall(), "mira-lanternfall.randoset", &L);
        e.extend(more);
        (layer, e)
    }

    /// The body under a header, checked: every message as printed.
    fn msgs(body: &str) -> Vec<String> {
        run(&format!("settings for lanternfall\n{body}"))
            .1
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn keys(body: &str) -> Vec<String> {
        run(&format!("settings for lanternfall\n{body}"))
            .1
            .into_iter()
            .filter_map(|d| d.key)
            .collect()
    }

    fn value(body: &str, key: &str) -> Val {
        let (l, e) = run(&format!("settings for lanternfall\n{body}"));
        assert!(e.is_empty(), "{e:#?}");
        l.values[key].val.clone()
    }

    fn plain(body: &str, key: &str) -> Value {
        match value(body, key) {
            Val::Plain(v) => v,
            v => panic!("{v:?}"),
        }
    }

    fn spec(body: &str, key: &str) -> Value {
        match value(body, key) {
            Val::Surprise(v) => v,
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn the_advanced_file_checks_clean_but_for_what_lanternfall_lacks() {
        let (_, e) = run(crate::ast::tests::ADVANCED);
        let e: Vec<String> = e.iter().map(ToString::to_string).collect();
        // `tricks` is not an option of the test world (doc 20 section 5.1 has no record kind
        // for world options), and the header asks for 0.1 of a world at 0.1.3, which passes.
        assert_eq!(e.len(), 1, "{e:#?}");
        assert!(e[0].contains("no setting called 'tricks'"), "{e:#?}");
    }

    /// Section 7.2's messages, one by one.
    #[test]
    fn messages_read_as_doc_20_section_7_2() {
        assert_eq!(
            msgs("shufle_mine = on\n"),
            [
                "mira-lanternfall.randoset:2:1: Lanternfall has no setting called 'shufle_mine'.\n  help: did you mean `shuffle_mine` (\"Shuffle the mine\")?"
            ]
        );
        assert_eq!(
            msgs("traps = many\n"),
            [
                "mira-lanternfall.randoset:2:9: 'Traps' can be none, a_few or lots. Your file says 'many'.\n  help: write one of: none, a_few, lots"
            ]
        );
        assert_eq!(
            msgs("traps = chance(none 50%, a_few 40%)\n"),
            [
                "mira-lanternfall.randoset:2:9: The chances for 'Traps' add up to 90. They need to add up to 100 (10 more)."
            ]
        );
        assert_eq!(
            msgs("goal = find_the_crown\nhint_count = 2\ngoal = light_every_lamp\n"),
            ["mira-lanternfall.randoset:4:1: 'Goal' is set twice, on lines 2 and 4. Keep one."]
        );
        assert_eq!(
            msgs("keep_home = [lantren]\n"),
            [
                "mira-lanternfall.randoset:2:14: 'lantren' is not an item in Lanternfall.\n  help: did you mean 'lantern'?"
            ]
        );
        assert_eq!(
            msgs("extra_tools = some_of(1 to 3, rope, boots, lamp_oil)\n"),
            [
                "mira-lanternfall.randoset:2:23: 'Extra tools' takes 0 to 2 entries, and some_of can pick 3.\n  help: write some_of(0 to 2, ...) or a smaller list"
            ]
        );
        let deathlink = msgs("deathlink = on\n");
        assert!(
            deathlink[0]
                .contains("did you mean `shared_deaths` (Shared deaths, also called deathlink)?"),
            "{deathlink:?}"
        );
    }

    #[test]
    fn a_common_key_the_world_does_not_offer_and_a_room_key_are_errors() {
        let s = Schema::parse(CINDERFALL, &L).unwrap();
        let (f, _) =
            parse("settings for cinderfall\nshared_deaths = on\nhint_price = 3\n", "f", &L);
        let e: Vec<String> = check(&f, &s, "f", &L).1.iter().map(ToString::to_string).collect();
        assert_eq!(
            e[0],
            "f:2:1: Cinderfall does not offer shared deaths.\n  help: remove this line; the game cannot use 'shared_deaths'"
        );
        assert!(e[1].starts_with("f:3:1: The room host decides 'Hint price'."));
        assert_eq!(keys("shared_deaths = on\n"), Vec::<String>::new());
    }

    #[test]
    fn each_kind_checks_and_comes_out_canonical() {
        assert_eq!(plain("shuffle_mine = true\n", "shuffle_mine"), json!(true));
        assert_eq!(plain("shuffle_mine = off\n", "shuffle_mine"), json!(false));
        assert_eq!(plain("lights = true\n", "lights"), json!("on"));
        assert_eq!(plain("bombs = plenty\n", "bombs"), json!(99));
        assert_eq!(plain("bombs = unlimited\n", "bombs"), json!(-1));
        assert_eq!(plain("bombs = 99\n", "bombs"), json!(99));
        assert_eq!(plain("shops = [tailor, bakery]\n", "shops"), json!(["bakery", "tailor"]));
        assert_eq!(plain("trap_mix = { ice = 70 }\n", "trap_mix"), json!({"ice": 70, "slow": 50}));
        assert_eq!(plain("greeting = \"Hi\"\n", "greeting"), json!("Hi"));
        assert_eq!(plain("title = \"Ana the Bold\"\n", "title"), json!({"text": "Ana the Bold"}));
        assert_eq!(plain("title = rogue\n", "title"), json!("rogue"));
        assert_eq!(
            plain("keep_home = [tools, lantern]\n", "keep_home"),
            json!(["boots", "lantern", "pickaxe", "rope"])
        );
        assert_eq!(
            plain("start_with = { items = [boots], from_shuffle = [arrow x 10] }\n", "start_with"),
            json!({"items": [["boots", 1]], "from_shuffle": [["arrow", 10]]})
        );
        assert_eq!(
            plain("no_important_at = [every_place]\n", "no_important_at").as_array().unwrap().len(),
            10
        );
        // NFC: a decomposed e-acute is stored composed.
        assert_eq!(plain("greeting = \"Caf\u{65}\u{301}\"\n", "greeting"), json!("Caf\u{e9}"));
    }

    #[test]
    fn bad_values_have_their_keys() {
        assert_eq!(keys("bombs = 100\n"), ["settings.out_of_range"]);
        assert_eq!(keys("bombs = lots\n"), ["settings.bad_value"]);
        assert_eq!(keys("shuffle_mine = [a]\n"), ["settings.bad_value"]);
        assert_eq!(keys("shops = [bakery, tailor, smithy, bakery]\n"), ["settings.bad_value"]);
        assert_eq!(keys("shops = []\n"), ["settings.out_of_range"]);
        assert_eq!(keys("shops = [mill]\n"), ["settings.bad_value"]);
        assert_eq!(keys("trap_mix = { slow = 101 }\n"), ["settings.out_of_range"]);
        assert_eq!(keys("trap_mix = { fire = 1 }\n"), ["settings.bad_value"]);
        assert_eq!(
            keys("greeting = \"this greeting is far too long\"\n"),
            ["settings.out_of_range"]
        );
        assert_eq!(keys("keep_home = [rope x 2]\n"), ["settings.bad_value"]);
        assert_eq!(keys("start_with = { items = [rope x all] }\n"), ["settings.placement"]);
        assert_eq!(keys("start_with = { from_shuffle = [rope x 2] }\n"), ["settings.placement"]);
        assert_eq!(keys("start_with = { item = [rope] }\n"), ["settings.bad_value"]);
        assert_eq!(keys("reveal_places = [early_places]\n"), ["settings.bad_value"]);
        assert_eq!(keys("reveal_places = [vault_alter]\n"), ["settings.unknown_name"]);
        assert_eq!(keys("lamp_color = blue\n"), ["settings.bad_value"]);
    }

    #[test]
    fn surprise_forms_keep_their_specs() {
        assert_eq!(spec("goal = any\n", "goal"), json!({"any": {}}));
        assert_eq!(
            spec("traps = one_of(none, a_few)\n", "traps"),
            json!({"one_of": ["a_few", "none"]})
        );
        assert_eq!(
            spec("traps = chance(none 50%, a_few 30%, lots 20%)\n", "traps"),
            json!({"chance": [["none", 50], ["a_few", 30], ["lots", 20]]})
        );
        assert_eq!(
            spec("hint_count = between(3, 8, low)\n", "hint_count"),
            json!({"between": [3, 8, "low"]})
        );
        assert_eq!(spec("hint_count = between(3, 8)\n", "hint_count"), json!({"between": [3, 8]}));
        assert_eq!(
            spec("hint_count = chance(5 60%, between(1, 3, low) 40%)\n", "hint_count"),
            json!({"chance": [[5, 60], [{"between": [1, 3, "low"]}, 40]]})
        );
        assert_eq!(
            spec("bombs = one_of(plenty, any)\n", "bombs"),
            json!({"one_of": [99, {"any": {}}]})
        );
        assert_eq!(
            spec("extra_tools = some_of(1 to 2, rope, boots, lamp_oil)\n", "extra_tools"),
            json!({"some_of": {"size": [1, 2], "names": ["boots", "lamp_oil", "rope"]}})
        );
        assert_eq!(
            spec("extra_tools = some_of(any, tools)\n", "extra_tools"),
            json!({"some_of": {"size": "any", "names": ["tools"]}})
        );
        assert_eq!(
            spec("extra_tools = some_of(2, tools)\n", "extra_tools"),
            json!({"some_of": {"size": [2, 2], "names": ["tools"]}})
        );
        assert_eq!(
            spec("greeting = chance(\"Hello\" 50%, \"Hi\" 50%)\n", "greeting"),
            json!({"chance": [["Hello", 50], ["Hi", 50]]})
        );
        assert_eq!(spec("shops = any\n", "shops"), json!({"any": {}}));
        assert_eq!(
            spec("title = one_of(hero, \"Ana\")\n", "title"),
            json!({"one_of": ["hero", {"text": "Ana"}]})
        );
    }

    #[test]
    fn bad_surprises_have_their_keys() {
        assert_eq!(keys("goal = one_of(find_the_crown)\n"), ["settings.surprise_shape"]);
        assert_eq!(
            keys("goal = one_of(find_the_crown, find_the_crown)\n"),
            ["settings.surprise_shape"]
        );
        assert_eq!(keys("hint_count = between(8, 3)\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("hint_count = between(3, 3)\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("hint_count = between(3, 30)\n"), ["settings.out_of_range"]);
        assert_eq!(keys("hint_count = between(3, 8, lowest)\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("goal = between(3, 8)\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("shops = one_of(bakery, tailor)\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("goal = some_of(1, find_the_crown)\n"), ["settings.surprise_shape"]);
        assert_eq!(
            keys("extra_tools = some_of(4, rope, boots, lamp_oil)\n"),
            ["settings.surprise_shape"]
        );
        assert_eq!(
            keys("extra_tools = some_of(2 to 1, rope, boots)\n"),
            ["settings.surprise_shape"]
        );
        assert_eq!(keys("extra_tools = some_of(1, rope, rope)\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("extra_tools = some_of(1, rpoe)\n"), ["settings.unknown_name"]);
        assert_eq!(keys("shops = some_of(any, bakery)\n"), Vec::<String>::new());
        assert_eq!(keys("greeting = any\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("trap_mix = any\n"), ["settings.surprise_shape"]);
        // Deeper than 2.2 allows is refused by the parser's depth cap, before it recurses.
        assert_eq!(keys("traps = one_of(none, one_of(a_few, lots))\n"), ["settings.limit"]);
        assert_eq!(keys("traps = chance(none 0%, a_few 100%)\n"), ["settings.chances_total"]);
        assert_eq!(keys("traps = chance(none 50%, none 50%)\n"), ["settings.surprise_shape"]);
    }

    #[test]
    fn blocks_check_each_line_and_their_own_rules() {
        let (l, e) = run(
            "settings for lanternfall\nwhen goal = light_every_lamp and traps = lots 50% {\n  hint_count = 10\n  keep_home += [lamp_oil]\n}\nwith 30% {\n  start_with = { items = [boots] }\n}\n",
        );
        assert!(e.is_empty(), "{e:#?}");
        assert_eq!(l.blocks.len(), 2);
        assert_eq!(
            l.blocks[0].when,
            [("goal".into(), json!("light_every_lamp")), ("traps".into(), json!("lots"))]
        );
        assert_eq!(l.blocks[0].changes[1].val, Val::Plain(json!(["lamp_oil"])));
        assert!(l.blocks[1].with);
        assert_eq!(
            keys("when goal = light_every_lamp {\n  goal = find_the_crown\n}\n"),
            ["settings.block_self"]
        );
        assert_eq!(
            keys("when goal = light_every_lamp {\n  hint_count = 1\n  hint_count = 2\n}\n"),
            ["settings.block"]
        );
        assert_eq!(
            keys("when goal = light_every_lamp {\n  hint_count += [a]\n}\n"),
            ["settings.block"]
        );
        assert_eq!(
            keys("when goal = light_every_lamp {\n  lamp_color = teal\n}\n"),
            ["settings.block"]
        );
        assert_eq!(keys("when lamp_color = teal {\n  music = epic\n}\n"), Vec::<String>::new());
        assert_eq!(keys("when goal = lost {\n  hint_count = 1\n}\n"), ["settings.bad_value"]);
        assert_eq!(
            keys("when goal = light_every_lamp {\n  hint_count = 50\n}\n"),
            ["settings.out_of_range"]
        );
        assert_eq!(
            keys("with 30% {\n  hint_count = 1\n}\nwith 30% {\n  hint_count = 1\n}\n"),
            ["settings.rule_duplicate"]
        );
        assert_eq!(keys("with 0% {\n  hint_count = 1\n}\n"), ["settings.chances_total"]);
        assert_eq!(
            plain_change("when goal = light_every_lamp {\n  trap_mix += { ice = 5 }\n}\n"),
            json!({"ice": 5})
        );
    }

    fn plain_change(body: &str) -> Value {
        let (l, e) = run(&format!("settings for lanternfall\n{body}"));
        assert!(e.is_empty(), "{e:#?}");
        match &l.blocks[0].changes[0].val {
            Val::Plain(v) => v.clone(),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn placements_check_names_and_copies() {
        let (l, e) = run(
            "settings for lanternfall\nplace pickaxe at mine_cart\nplace [potion x 3, rope] at [p1, p2, p3, p4]\nplace arrow x all at cave_places copy\nplace some_of(2, bomb_bag, lamp_oil, potion) at early_places if_possible\nconnect cave_mouth to one_of(mill_door, well_door) way_out 50%\npin stone_golem at mill_arena in bosses\n",
        );
        let e: Vec<String> = e.iter().map(ToString::to_string).collect();
        assert_eq!(
            e,
            [
                "mira-lanternfall.randoset:4:7: `x all` takes copies from the shuffle; it cannot be a `copy`."
            ]
        );
        assert_eq!(l.place[0].single.as_deref(), Some("mine_cart"));
        assert_eq!(
            l.place[1].json,
            json!({"items": [["potion", 3], ["rope", 1]], "places": ["p1", "p2", "p3", "p4"], "copy": false, "if_possible": false, "chance": 100})
        );
        assert_eq!(
            l.place[2].json["items"],
            json!({"some_of": {"size": [2, 2], "names": ["bomb_bag", "lamp_oil", "potion"]}})
        );
        assert!(!l.place[2].strict);
        assert_eq!(
            l.connect[0].0,
            json!({"from": "cave_mouth", "to": {"one_of": ["mill_door", "well_door"]}, "dir": "way_out", "chance": 50})
        );
        assert_eq!(
            l.pin[0].0,
            json!({"thing": "stone_golem", "slot": "mill_arena", "shuffle": "bosses"})
        );
        assert_eq!(keys("place [potion x 3] at [p1, p2]\n"), ["settings.placement"]);
        assert_eq!(keys("place rope x 2 at mine_cart\n"), ["settings.placement"]);
        assert_eq!(keys("place ropee at mine_cart\n"), ["settings.unknown_name"]);
        assert_eq!(keys("place rope at mine_kart\n"), ["settings.unknown_name"]);
        assert_eq!(keys("connect cave_mouth to mil_door\n"), ["settings.unknown_name"]);
        assert_eq!(keys("pin stone_golem at mill_arena in boss\n"), ["settings.unknown_name"]);
        assert_eq!(keys("pin golem at mill_arena in bosses\n"), ["settings.placement"]);
        assert_eq!(keys("place one_of(rope, boots) at [p1, p2] 0%\n"), ["settings.chances_total"]);
    }

    #[test]
    fn the_header_is_checked() {
        let k = |src: &str| -> Vec<String> {
            let (f, mut e) = parse(src, "f", &L);
            e.extend(check(&f, &lanternfall(), "f", &L).1);
            e.into_iter().filter_map(|d| d.key).collect()
        };
        assert_eq!(k("format 2\nsettings for lanternfall\n"), ["settings.format"]);
        assert_eq!(k("settings for cinderfall\n"), ["settings.world_not_found"]);
        assert_eq!(k("settings for lanternfall version \"0.2\"\n"), ["settings.game_version"]);
        assert_eq!(k("settings for lanternfall version \"=0.1.3\"\n"), Vec::<String>::new());
        assert_eq!(
            k("settings for lanternfall version \"not a version\"\n"),
            ["settings.game_version"]
        );
        assert_eq!(k("preset fast for lanternfall\nplayer \"Mira\"\n"), ["settings.header"]);
        assert_eq!(k("room settings\n"), ["settings.header"]);
        assert_eq!(k("settings for lanternfall\nplayer \"Mira\u{202e}\"\n"), ["settings.limit"]);
        let long = "a".repeat(65);
        assert_eq!(
            k(&format!("settings for lanternfall\nplayer \"{long}\"\n")),
            ["settings.limit"]
        );
        let at = "\u{e9}".repeat(64);
        assert_eq!(
            k(&format!("settings for lanternfall\nplayer \"{at}\"\n")),
            Vec::<String>::new()
        );
        let msg = run("settings for lanternfall version \"0.2\"\n").1[0].to_string();
        assert_eq!(
            msg,
            "mira-lanternfall.randoset:1:34: This file is for Lanternfall 0.2. The world found is 0.1.3.\n  help: use Lanternfall 0.2, or remove `version \"0.2\"` to accept any version"
        );
    }

    #[test]
    fn group_expansion_and_copies_are_capped() {
        let small = Limits { settings_max_expanded_names: 3, settings_max_total_copies: 5, ..L };
        let k = |body: &str| -> Vec<String> {
            let (f, _) = parse(&format!("settings for lanternfall\n{body}"), "f", &small);
            check(&f, &lanternfall(), "f", &small).1.into_iter().filter_map(|d| d.key).collect()
        };
        assert_eq!(k("keep_home = [tools]\n"), Vec::<String>::new());
        assert_eq!(k("keep_home = [tools, lantern]\n"), ["settings.limit"]);
        assert_eq!(k("start_with = { items = [arrow x 5] }\n"), Vec::<String>::new());
        assert_eq!(k("start_with = { items = [arrow x 6] }\n"), ["settings.limit"]);
        let _ = vec![0];
    }

    /// Review of PR 233, high 2: expansion stops at the cap, before a group is copied, and
    /// stays stopped for the rest of the file.
    #[test]
    fn expansion_stops_at_the_cap_within_a_time_budget() {
        let items: Vec<String> =
            (0..50_000).map(|i| format!("{{\"name\":\"i{i}\",\"copies\":1}}")).collect();
        let schema = format!(
            "{{\"format\":\"schema/1\",\"world\":\"big\",\"version\":\"1.0.0\",\"items\":[{}]}}",
            items.join(",")
        );
        let s = Schema::parse(&schema, &L).unwrap();
        let src = format!("settings for big\n{}", "reveal_items = [every_item]\n".repeat(2000));
        assert!(src.len() > 56_000);
        let (f, _) = parse(&src, "f", &L);
        let start = std::time::Instant::now();
        let (_, e) = check(&f, &s, "f", &L);
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_millis(500), "took {took:?}");
        assert_eq!(e.iter().filter(|d| d.key.as_deref() == Some("settings.limit")).count(), 1);
    }

    /// Review of PR 233, medium 6 and mutation survivor M13: each `place chance` entry is 1 to
    /// 100%, an item is drawn once, and the chances add to 100.
    #[test]
    fn place_chance_checks_each_entry_and_the_total() {
        assert_eq!(
            keys("place chance(rope 150%, boots -50%) at mine_cart\n"),
            ["settings.chances_total"]
        );
        assert_eq!(
            keys("place chance(rope 50%, rope 50%) at mine_cart\n"),
            ["settings.surprise_shape"]
        );
        assert_eq!(
            keys("place chance(rope 50%, boots 40%) at mine_cart\n"),
            ["settings.chances_total"]
        );
        assert_eq!(keys("place one_of(rope, rope) at mine_cart\n"), ["settings.surprise_shape"]);
        assert_eq!(keys("place chance(rope 50%, boots 50%) at mine_cart\n"), Vec::<String>::new());
    }

    /// Review of PR 233, low 11: a block tests an option once, draws are among distinct
    /// doors, and a `some_of` pool holds at least one name.
    #[test]
    fn smaller_gaps_are_errors() {
        assert_eq!(
            keys("when goal = find_the_crown and goal = light_every_lamp {\n  hint_count = 1\n}\n"),
            ["settings.block"]
        );
        assert_eq!(
            keys("connect cave_mouth to one_of(mill_door, mill_door)\n"),
            ["settings.surprise_shape"]
        );
        let empty = r#"{"format":"schema/1","world":"w","version":"1.0.0","options":[{"key":"t","kind":"set","of":"items","default":[]}],"items":[{"name":"a","copies":1}],"item_groups":[{"name":"none","members":[]}]}"#;
        let s = Schema::parse(empty, &L).unwrap();
        let k = |body: &str| -> Vec<String> {
            let (f, _) = parse(&format!("settings for w\n{body}"), "f", &L);
            check(&f, &s, "f", &L).1.into_iter().filter_map(|d| d.key).collect()
        };
        assert_eq!(k("t = some_of(any, none)\n"), ["settings.surprise_shape"]);
        assert_eq!(
            k("place some_of(any, none) at x\n").first().map(String::as_str),
            Some("settings.surprise_shape")
        );
    }

    /// Review of PR 233, medium 8: nothing written is dropped silently (D-121).
    #[test]
    fn repeats_and_stray_counts_are_errors() {
        assert_eq!(
            keys("start_with = { items = [rope], items = [boots] }\n"),
            ["settings.bad_value"]
        );
        assert_eq!(keys("place rope at [mine_cart x 5]\n"), ["settings.bad_value"]);
        assert_eq!(keys("place rope at mine_cart x 5\n"), ["settings.syntax"]);
        assert_eq!(keys("legacy {\n  a = 1\n  a = 2\n}\n"), ["settings.set_twice"]);
        assert_eq!(keys("legacy {\n  a = 1\n}\nlegacy {\n  a = 2\n}\n"), ["settings.set_twice"]);
        assert_eq!(
            keys("when goal = light_every_lamp {\n  trap_mix += { slow = 1, slow = 2 }\n}\n"),
            ["settings.bad_value"]
        );
        let no_default = r#"{"format":"schema/1","world":"w","version":"1","options":[{"key":"a","kind":"toggle"}]}"#;
        assert!(Schema::parse(no_default, &L).unwrap_err().contains("default"));
    }

    #[test]
    fn legacy_lines_pass_through() {
        let (l, e) = run(
            "settings for lanternfall\nlegacy {\n  old_mode = fast\n  old_list = [a, b x 2]\n}\n",
        );
        assert!(e.is_empty(), "{e:#?}");
        assert_eq!(
            Value::Object(l.legacy),
            json!({"old_mode": "fast", "old_list": ["a", ["b", 2]]})
        );
        assert_eq!(keys("legacy {\n  a = any\n}\n"), ["settings.bad_value"]);
    }
}
