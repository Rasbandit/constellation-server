//! Hints on the wire (docs/protocol-plan.md section 2): the `Hint` record and its print, the
//! `!hint` and `!hint_location` name lookup, and the command result texts. The room owns the
//! hints and the points; this module only translates.
//!
//! Deviations from the reference server (docs/compat-status.md): names match exactly or, when
//! one name matches, ignoring case, with no fuzzy matching or "did you mean" suggestion; ties
//! between equally good candidates go to the lowest (slot, location), not a seeded shuffle.
#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]

use crate::CompatRoom;
use crate::wire::{self, NetworkItem, PrintJson, TextPart};
use proto::{HintAsk, SlotNo};
use room_core::{Hint, Hinted, hint_status};
use seed_bundle::{SlotView, package::Names};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// A hint as the protocol sends it.
pub(crate) fn record(room: &CompatRoom, h: &Hint) -> wire::Hint {
    wire::Hint {
        receiving_player: h.receiver.0,
        finding_player: h.finding.0,
        location: h.location,
        item: h.item,
        found: h.found,
        entrance: entrance(room, h).unwrap_or_default().to_owned(),
        item_flags: room.flags(h.finding.0, h.location),
        status: h.status,
    }
}

fn entrance<'a>(room: &'a CompatRoom, h: &Hint) -> Option<&'a str> {
    room.bundle.slot(h.finding.0)?.entrance(h.location)
}

/// The value of a `_read_hints_` key.
pub(crate) fn list(room: &CompatRoom, hints: &[Hint]) -> Value {
    json!(hints.iter().map(|h| record(room, h)).collect::<Vec<_>>())
}

/// The `Hint` print: the receiver's item is at the finder's location, then the status (the
/// part layout of the protocol doc's `JSONMessagePart` types; reference `NetUtils.py:421-442`).
pub(crate) fn print(room: &CompatRoom, h: &Hint) -> PrintJson {
    let (receiver, finder) = (h.receiver.0, h.finding.0);
    let flags = room.flags(finder, h.location);
    let mut data = vec![
        TextPart::text("Hint: "),
        TextPart::player(receiver),
        TextPart::text("'s "),
        TextPart::item(h.item, receiver, flags),
        TextPart::text(" is at "),
        TextPart::location(h.location, finder),
        TextPart::text(" in "),
        TextPart::player(finder),
        TextPart::text("'s world"),
    ];
    if let Some(e) = entrance(room, h) {
        data.extend([TextPart::text(" at "), TextPart::entrance(e)]);
    }
    data.extend([TextPart::text(". "), TextPart::hint_status(h.status)]);
    PrintJson {
        kind: "Hint".to_owned(),
        data: json!(data),
        receiving: Some(receiver),
        found: Some(h.found),
        item: Some(NetworkItem { item: h.item, location: h.location, player: finder, flags }),
        ..PrintJson::default()
    }
}

/// The hints of a batch that `slot` sees, in the order the reference sends them
/// (`MultiServer.py:901-929`): those it finds first, found ones first within each part.
pub(crate) fn for_slot(slot: u32, hints: &[Hint]) -> Vec<Hint> {
    let mut mine: Vec<Hint> =
        hints.iter().filter(|h| h.finding.0 == slot || h.receiver.0 == slot).copied().collect();
    mine.sort_by_key(|h| (h.finding.0 != slot, !h.found));
    mine
}

/// `!hint` (items) or `!hint_location` (locations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An item or an item group.
    Item,
    /// A location or a location group of the asker's world.
    Location,
}

impl Kind {
    const fn noun(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Location => "location",
        }
    }
}

/// What `slot` asks for with `arg`: every location holding the item (or the item group's
/// items) for it, or the location (or group) in its own world, most wanted first. `Err` is the
/// text to answer with.
pub(crate) fn resolve(
    room: &CompatRoom,
    slot: &SlotView<'_>,
    arg: &str,
    kind: Kind,
) -> Result<Vec<HintAsk>, String> {
    let (game, g) = (slot.game(), slot.package());
    let (ids, names) = match kind {
        Kind::Item => (g.items(), g.item_names()),
        Kind::Location => (g.locations(), g.location_names()),
    };
    let wanted: BTreeSet<i64> = if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
        let id: i64 = arg.parse().map_err(|_| format!("{arg} is not an id"))?;
        let name = ids.binary_search(&id).ok().and_then(|i| names.name(i));
        if let Some(name) = name.filter(|n| g.hint_blacklisted(n)) {
            return Err(not_hintable(name));
        }
        BTreeSet::from([id])
    } else {
        let name = lookup(names, arg)
            .ok_or_else(|| format!("No {0} or {0} group named '{arg}' in {game}.", kind.noun()))?;
        if g.hint_blacklisted(name) {
            return Err(not_hintable(name));
        }
        // A group name wins over an item of the same name, as in the reference.
        let members = names.group(name).map_or_else(
            || names.index(name).into_iter().collect::<Vec<_>>(),
            |m| m.iter().map(|&i| i as usize).collect(),
        );
        members.into_iter().filter_map(|i| ids.get(i).copied()).collect()
    };
    // (finding slot, location, flags, receiver) of every placement asked for.
    let at: Vec<(SlotView<'_>, i64, u8, u32)> = match kind {
        // shortcut: scans every placement of the room per `!hint` rather than keep an item
        // index per room (memory every room would pay); index it if big rooms make it slow.
        Kind::Item => room
            .bundle
            .slots()
            .flat_map(|f| f.placements().map(move |p| (f, p)))
            .filter(|(_, p)| p.receiver == slot.id() && wanted.contains(&p.item))
            .map(|(f, p)| (f, p.location, p.flags, p.receiver))
            .collect(),
        Kind::Location => wanted
            .iter()
            .filter_map(|&l| slot.placement(slot.find(l)?))
            .map(|p| (*slot, p.location, p.flags, p.receiver))
            .collect(),
    };
    let mut asks: Vec<(HintAsk, (u32, bool))> = at
        .into_iter()
        .map(|(finder, location, flags, receiver)| {
            // Item hints start as priority, or avoid for a trap; location hints as unspecified
            // (`MultiServer.py:1289-1296, 1306`).
            let status = match kind {
                Kind::Item if flags & 4 != 0 => hint_status::AVOID,
                Kind::Item => hint_status::PRIORITY,
                Kind::Location => hint_status::UNSPECIFIED,
            };
            let ask = HintAsk { finding: SlotNo(finder.id()), location, status };
            let sphere = finder.sphere(location).unwrap_or(u32::MAX);
            (ask, (sphere, receiver == finder.id()))
        })
        .collect();
    // Earliest sphere first, then another player's world (`MultiServer.py:1858-1871`).
    asks.sort_by_key(|(a, (sphere, local))| (*sphere, *local, a.finding, a.location));
    Ok(asks.into_iter().map(|(a, _)| a).collect())
}

/// The name `arg` means: a group or name equal to it (a binary search), else the only one
/// equal ignoring case (one pass over the names, nothing allocated per name).
fn lookup<'a>(names: &'a Names, arg: &str) -> Option<&'a str> {
    if let Some(n) = names.stored(arg) {
        return Some(n);
    }
    let lower = arg.to_lowercase();
    let mut same = names.all().filter(|n| n.chars().flat_map(char::to_lowercase).eq(lower.chars()));
    let first = same.next()?;
    same.next().is_none().then_some(first)
}

fn not_hintable(name: &str) -> String {
    format!("'{name}' cannot be hinted in this game.")
}

/// The command result after a `!hint` or `!hint_location`, if any (the reference says
/// nothing when every new hint was paid for).
pub(crate) fn outcome(h: &Hinted, asked_for: &str) -> Option<String> {
    let afford = format!("You have {} points and need {} for a hint.", h.points, h.cost);
    let can_pay = h.points >= i64::try_from(h.cost).unwrap_or(i64::MAX);
    if h.shown.is_empty() && h.unpaid == 0 {
        return Some(if can_pay {
            format!("Nothing in this multiworld matches '{asked_for}'.")
        } else {
            afford
        });
    }
    if h.unpaid == 0 {
        return h
            .shown
            .iter()
            .all(|s| !s.new)
            .then(|| "That hint was already known; no points were spent.".to_owned());
    }
    Some(if h.shown.is_empty() {
        afford
    } else if can_pay {
        "There may be more; run the command again for another.".to_owned()
    } else {
        format!("There may be more, but you have {} points and need {}.", h.points, h.cost)
    })
}

/// The command result of a bare `!hint`.
pub(crate) fn summary(cost: u64, points: i64) -> String {
    format!("A hint costs {cost} points; you have {points}.")
}

#[cfg(test)]
mod tests {
    use super::lookup;
    use seed_bundle::{GameData, GamePackage};
    use std::collections::BTreeMap;

    #[test]
    fn a_name_matches_exactly_else_the_only_one_equal_ignoring_case() {
        let ids = BTreeMap::from([("Bow".into(), 1), ("bow".into(), 2), ("Sword".into(), 3)]);
        let g = GameData { item_name_to_id: ids, ..GameData::default() };
        let p = GamePackage::new([0; 32], "G", &g);
        let found = |arg| lookup(p.item_names(), arg);
        assert_eq!(found("bow"), Some("bow"), "exact wins");
        assert_eq!(found("SWORD"), Some("Sword"));
        assert_eq!(found("BOW"), None, "two names equal ignoring case: ambiguous");
        assert_eq!(found("Swor"), None);
        assert_eq!(found(""), None);
    }
}
