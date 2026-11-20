//! The several-player run of `port-check`: the legacy side generates a room (one world, a
//! player each with their own settings), and the same four checks run over it. See
//! `docs/port-check.md`, "Several players".

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use data_fill::check::{SeedView, Shape, compare_shapes, diff_spheres, fnv1a};
use data_fill::room::{self, Given, Seat, cross_world_share, fill_room_spheres, locked_placed};
use data_fill::spheres::{RoomSpheres, room_spheres};
use data_fill::{Plan, World};
use serde_json::{Value, json};
use spec_rng::SpecRng;

use super::{
    Args, Ctx, FORMAT, Legacy, R, Seed, Tally, fetch_instance, peak_rss_kb, reachability, write,
};

/// One slot's placements in key form: location to `{"item": key, "owner": slot}`.
type Slot = BTreeMap<String, Value>;

/// The compiled room of one seed: every player's instance and plan.
struct Room<'w> {
    seeds: Vec<Seed<'w>>,
}

impl<'w> Room<'w> {
    fn load(legacy: &mut Legacy, w: &'w World, a: &Args, seed: u64, players: u64) -> R<Self> {
        let insts = (1..=players)
            .map(|p| fetch_instance(legacy, &a.out, seed, Some(p)))
            .collect::<R<Vec<_>>>()?;
        let pairs: Vec<(&World, &_)> = insts.iter().map(|i| (w, i)).collect();
        let plans =
            room::plans(&pairs, config::FILL_SETUP_WORKERS).map_err(|e| format!("{e:?}"))?;
        let seeds = insts
            .into_iter()
            .zip(plans)
            .map(|(inst, plan): (_, Plan)| {
                let progression = inst.progression(w);
                Seed { seed, inst, w, plan, progression }
            })
            .collect();
        Ok(Room { seeds })
    }

    fn seats(&self) -> Vec<Seat<'_>> {
        self.seeds
            .iter()
            .map(|s| Seat { w: s.w, plan: &s.plan, progression: &s.progression })
            .collect()
    }

    /// `p<player>:<key>`, the name of a location or item in a room's flat lists.
    fn flat(p: usize, key: &str) -> String {
        format!("p{}:{key}", p + 1)
    }

    /// Placements from the legacy side's shape (`{slot: {location: {item, owner}}}`).
    fn parse(&self, v: &Value) -> R<Vec<Vec<Option<Given>>>> {
        let seats = self.seats();
        let mut placed = locked_placed(&seats);
        for (p, s) in seats.iter().enumerate() {
            let Some(slot) = v.get((p + 1).to_string()).and_then(Value::as_object) else {
                return Err(format!("room placements lack slot {}", p + 1));
            };
            placed[p].fill(None);
            for (l, g) in slot {
                let owner = g["owner"].as_u64().and_then(|o| usize::try_from(o).ok());
                let owner = owner
                    .filter(|o| (1..=seats.len()).contains(o))
                    .ok_or_else(|| format!("slot {} {l}: bad owner {}", p + 1, g["owner"]))?;
                let item = g["item"].as_str().ok_or("placement without an item")?;
                placed[p][s.w.loc(l)? as usize] = Some(Given {
                    to: u16::try_from(owner - 1).unwrap_or(0),
                    item: seats[owner - 1].w.item(item)?,
                });
            }
        }
        Ok(placed)
    }

    /// Our placements in the legacy side's shape.
    fn slots(&self, placed: &[Vec<Option<Given>>]) -> Vec<Slot> {
        let seats = self.seats();
        placed
            .iter()
            .enumerate()
            .map(|(p, row)| {
                row.iter()
                    .enumerate()
                    .filter_map(|(l, g)| {
                        let g = (*g)?;
                        let key = seats[p].w.loc_key(u32::try_from(l).ok()?).to_owned();
                        let item = seats[usize::from(g.to)].w.item_key(g.item);
                        Some((key, json!({"item": item, "owner": g.to + 1})))
                    })
                    .collect()
            })
            .collect()
    }

    /// Spheres as flat `p<player>:<location>` keys.
    fn keys(&self, s: &RoomSpheres) -> Vec<Vec<String>> {
        s.spheres
            .iter()
            .map(|x| {
                x.iter()
                    .map(|(p, l)| {
                        Self::flat(usize::from(*p), self.seeds[usize::from(*p)].w.loc_key(*l))
                    })
                    .collect()
            })
            .collect()
    }

    /// The flat map the shape statistics read: `p<slot>:<location>` to `p<owner>:<item>`.
    fn flat_map(&self, slots: &[Slot]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (p, slot) in slots.iter().enumerate() {
            for (l, g) in slot {
                let o = g["owner"].as_u64().unwrap_or(1) as usize - 1;
                m.insert(Self::flat(p, l), Self::flat(o, g["item"].as_str().unwrap_or("")));
            }
        }
        m
    }

    /// `p<owner>:<item>` to the number of progression copies (locked ones included).
    fn copies(&self) -> HashMap<String, usize> {
        let mut c = HashMap::new();
        for (p, s) in self.seeds.iter().enumerate() {
            for k in s.progression_items() {
                *c.entry(Self::flat(p, &k)).or_default() += 1;
            }
        }
        c
    }

    fn shape(&self, shape: &mut Shape, cx: &Ctx, spheres: &[Vec<String>], slots: &[Slot]) {
        let placements = self.flat_map(slots);
        let counts = self.copies();
        let split = |k: &str| -> Option<(usize, String)> {
            let (p, key) = k.strip_prefix('p')?.split_once(':')?;
            Some((p.parse::<usize>().ok()? - 1, key.to_owned()))
        };
        let progression = |k: &str| split(k).is_some_and(|(p, key)| self.seeds[p].prog_key(&key));
        let key_item = |k: &str| counts.get(k).is_some_and(|n| *n <= 2);
        let owner = |l: &str| split(l).and_then(|(_, key)| cx.rules.owner(&key).map(str::to_owned));
        shape.add(&SeedView {
            spheres,
            placements: &placements,
            progression: &progression,
            key_item: &key_item,
            owner: &owner,
        });
    }
}

/// Compare the legacy sphere answer for `slots` with ours.
fn spheres_check(
    room: &Room<'_>,
    seed: u64,
    slots: &[Slot],
    legacy: &Value,
    ours: &RoomSpheres,
) -> (bool, Option<Value>, Vec<Vec<String>>, bool) {
    let ours_keys = room.keys(ours);
    let legacy_keys: Vec<Vec<String>> = legacy["spheres"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            s.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| {
                    let (p, l) = (x[0].as_u64()?, x[1].as_str()?);
                    Some(Room::flat(usize::try_from(p).ok()? - 1, l))
                })
                .collect()
        })
        .collect();
    let legacy_beatable = legacy["beatable"] == json!(true);
    let violations = legacy["violations"].as_array().cloned().unwrap_or_default();
    let diff = diff_spheres(&legacy_keys, &ours_keys);
    let ok = diff.is_none() && legacy_beatable == ours.all_beatable() && violations.is_empty();
    let detail = (!ok).then(|| {
        let mut d = json!({"seed": seed, "legacy_beatable": legacy_beatable,
                           "port_beatable": ours.all_beatable(), "port_beatable_by_player": ours.beatable,
                           "violations": violations});
        if let Some((round, loc, side)) = &diff {
            d["sphere"] = json!(round + 1);
            d["location"] = json!(loc);
            d["reached_by"] = json!(side);
            if let Some((p, l)) = loc.strip_prefix('p').and_then(|x| x.split_once(':')) {
                let slot = p.parse::<usize>().ok().and_then(|p| slots.get(p - 1));
                d["item_there"] = json!(slot.and_then(|s| s.get(l)));
            }
        }
        d
    });
    (ok, detail, legacy_keys, legacy_beatable)
}

/// Mean per player of a list of per-seed shares.
fn means(rows: &[Vec<f64>], players: usize) -> Vec<f64> {
    (0..players)
        .map(|p| rows.iter().map(|r| r[p]).sum::<f64>() / rows.len().max(1) as f64)
        .collect()
}

fn shares(seats: &[Seat<'_>], placed: &[Vec<Option<Given>>]) -> Vec<f64> {
    cross_world_share(seats, placed)
        .iter()
        .map(|(x, all)| *x as f64 / (*all).max(1) as f64)
        .collect()
}

#[allow(clippy::too_many_lines)] // the run is one sequence of checks
pub(super) fn run(
    a: &Args,
    world: &World,
    legacy: &mut Legacy,
    cx: &mut Ctx,
    hello: Value,
    players: u64,
    t_all: Instant,
) -> R<Value> {
    let version =
        json!({"engine": format!("data-fill {}", env!("CARGO_PKG_VERSION")), "format": FORMAT});
    let (mut reach, mut replay, mut judged, mut determinism) =
        (Tally::default(), Tally::default(), Tally::default(), Tally::default());
    let (mut shape_legacy, mut shape_ours) = (Shape::default(), Shape::default());
    let (mut share_legacy, mut share_ours): (Vec<Vec<f64>>, Vec<Vec<f64>>) =
        (Vec::new(), Vec::new());
    let (mut replay_beatable, mut judged_beatable) = (0u64, 0u64);
    let (mut hashes, mut legacy_failures): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
    let k = a.seeds.max(1);
    let units = usize::try_from(k * players).unwrap_or(1);
    for n in 0..k {
        let seed = a.first_seed + n;
        eprintln!("port-check: room seed {seed} ({}/{k}, {players} players)", n + 1);
        let t = Instant::now();
        let room = Room::load(legacy, world, a, seed, players)?;
        cx.engine_seconds += t.elapsed().as_secs_f64();
        let seats = room.seats();

        // (1) Per-player reachability, the inventories spread over every seed and player.
        for (p, s) in room.seeds.iter().enumerate() {
            let idx = usize::try_from(n * players).unwrap_or(0) + p;
            let share = a.inventories / units + usize::from(idx < a.inventories % units);
            reachability(legacy, cx, &mut reach, s, share, Some(p as u64 + 1))?;
        }

        // (2) The legacy room, replayed in our engine.
        let generated = match legacy.call(&json!({"op": "room_generate", "seed": seed})) {
            Ok(g) => g,
            Err(e) => {
                legacy_failures.push(json!([seed, e]));
                continue;
            }
        };
        let theirs = room.parse(&generated["placements"])?;
        let their_slots = room.slots(&theirs);
        write(
            &a.out.join(format!("legacy/room_placements_{seed}.json")),
            serde_json::to_string(&their_slots).unwrap_or_default().as_bytes(),
        )?;
        let lsph = legacy.call(
            &json!({"op": "room_spheres", "seed": seed, "placements": slot_map(&their_slots)}),
        )?;
        let t = Instant::now();
        let mine = room_spheres(&seats, &theirs);
        cx.engine_seconds += t.elapsed().as_secs_f64();
        let (ok, detail, lkeys, lbeat) = spheres_check(&room, seed, &their_slots, &lsph, &mine);
        replay.record(ok, || detail.unwrap_or_default());
        replay_beatable += u64::from(lbeat);
        room.shape(&mut shape_legacy, cx, &lkeys, &their_slots);
        share_legacy.push(shares(&seats, &theirs));

        // (3) + (4) Our room: three fills give the same bytes, then the legacy side judges it.
        if n < a.ours {
            let fill_seed = a.fill_seed_base + seed;
            let (mut files, mut ours, mut model) = (Vec::new(), Vec::new(), None);
            for _ in 0..3 {
                let t = Instant::now();
                let (filled, sp) = fill_room_spheres(
                    &seats,
                    &mut SpecRng::from_u64(fill_seed),
                    config::FILL_MAX_ATTEMPTS,
                )
                .map_err(|e| format!("fill {seed}: {e:?}"))?;
                cx.engine_seconds += t.elapsed().as_secs_f64();
                cx.work.0.absorb(&filled.stats);
                cx.work.1 += 1;
                cx.work.2 += u64::from(filled.solves);
                let slots = room.slots(&filled.placed);
                let file = json!({"version": version, "instance_seed": seed, "fill_seed": fill_seed,
                                  "players": players, "placements": slots});
                files.push(serde_json::to_vec_pretty(&file).unwrap_or_default());
                ours = filled.placed;
                model = Some(sp);
            }
            let same = files.windows(2).all(|p| p[0] == p[1]);
            determinism.record(same, || json!({"seed": seed, "hashes": files.iter().map(|f| format!("{:016x}", fnv1a(f))).collect::<Vec<_>>()}));
            hashes.push(json!([fill_seed, format!("{:016x}", fnv1a(&files[0]))]));
            write(&a.out.join(format!("ours/room_placements_{seed}.json")), &files[0])?;
            let our_slots = room.slots(&ours);
            let lsph = legacy.call(
                &json!({"op": "room_spheres", "seed": seed, "placements": slot_map(&our_slots)}),
            )?;
            let model = model.unwrap_or_else(|| room_spheres(&seats, &ours));
            let (ok, detail, lkeys, lbeat) = spheres_check(&room, seed, &our_slots, &lsph, &model);
            judged.record(ok, || detail.unwrap_or_default());
            judged_beatable += u64::from(lbeat);
            room.shape(&mut shape_ours, cx, &lkeys, &our_slots);
            share_ours.push(shares(&seats, &ours));
        }
    }
    let checks = json!({
        "reachability": reach.json(json!({"what": "legacy solver vs port engine per player: locations, regions with time masks, event counts"})),
        "legacy_seeds_replayed": replay.json(json!({"what": "legacy rooms: spheres over the whole room and beatability agree", "beatable_by_legacy": replay_beatable, "legacy_generation_failures": legacy_failures})),
        "port_seeds_judged_by_legacy": judged.json(json!({"what": "port rooms: legacy finds them valid, beatable, same spheres", "beatable_by_legacy": judged_beatable})),
        "determinism": determinism.json(json!({"what": "same seed + same version = same bytes (3 fills each)", "version": version, "hashes": hashes})),
    });
    let pass =
        ["reachability", "legacy_seeds_replayed", "port_seeds_judged_by_legacy", "determinism"]
            .iter()
            .all(|c| checks[c]["pass"] == json!(true));
    let p = usize::try_from(players).unwrap_or(1);
    let (ml, mo) = (means(&share_legacy, p), means(&share_ours, p));
    let mut shape = compare_shapes(&shape_legacy, &shape_ours);
    let gaps: Vec<String> = (0..p)
        .filter(|i| (ml[*i] - mo[*i]).abs() > 0.15)
        .map(|i| {
            format!(
                "player {}: share of progression in other worlds, legacy {:.2}, port {:.2}",
                i + 1,
                ml[i],
                mo[i]
            )
        })
        .collect();
    shape["cross_world_progression_share (per player, mean over seeds)"] = json!({
        "legacy": ml, "port": mo, "flags": gaps,
        "what": "share of each player's progression items that lie in other players' worlds"});
    if let Some(f) = shape["flags"].as_array_mut() {
        f.extend(gaps.iter().map(|g| json!(g)));
    }
    Ok(json!({
        "pass": pass,
        "players": players,
        "checks": checks,
        "seed_shape (informational)": shape,
        "port_sanity (informational)": "not run for rooms; fill-fairness measures a room (docs/port-check.md)",
        "work": cx.work.0.summary(cx.work.1, cx.work.2),
        "seconds": {"total": t_all.elapsed().as_secs_f64(), "legacy": legacy.seconds, "port_engine": cx.engine_seconds},
        "port_engine_peak_rss_kb": peak_rss_kb(),
        "legacy_adapter": hello,
    }))
}

/// Slots keyed by player number, the form the legacy side takes.
fn slot_map(slots: &[Slot]) -> Value {
    Value::Object(slots.iter().enumerate().map(|(p, s)| ((p + 1).to_string(), json!(s))).collect())
}
