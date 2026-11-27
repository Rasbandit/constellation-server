//! `rando generate`: a built world filled into a seed by the engine's own room fill
//! (`data_fill::seed::generate_on`) and written with the engine's seed writer
//! (`data_fill::seed::encode_on`), plus a spoiler log read back from the seed bundle. Nothing
//! here places an item: this module only asks the engine and reports what it did.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use data_fill::FillError;
use data_fill::prepared::{self, Prepared, SEED_MAX_BYTES};
use data_fill::seed::{FillReport, GenerateError, Player, encode_on, generate_on};
use seed_bundle::SeedBundle;
use serde_json::{Value, json};

use crate::Built;

/// A built world, prepared once for the engine: what every seed of it starts from.
pub struct Generator<'b> {
    built: &'b Built,
    prepared: Prepared,
    /// Location key to the region that holds it.
    regions: HashMap<String, String>,
}

/// One filled seed.
#[derive(Debug, Clone)]
pub struct Seed {
    /// The seed's random stream, which reproduces it.
    pub seed: u64,
    /// The seed's name (`<game>-<seed>`), also its file name.
    pub name: String,
    /// The seed file (v2) a room runs on.
    pub file: Vec<u8>,
    /// The spoiler log as data: `spoiler_text` writes it for people.
    pub spoiler: Value,
    /// How the fill went.
    pub report: FillReport,
}

impl<'b> Generator<'b> {
    /// Prepare `built` for filling (compile it into the engine's stored form).
    ///
    /// # Errors
    /// What the engine refuses in the manifest.
    pub fn new(built: &'b Built) -> Result<Self, String> {
        let (file, _) = prepared::build(&built.manifest)?;
        let prepared = Prepared::open(&file)?;
        let mut regions = HashMap::new();
        for r in built.manifest["regions"].as_array().into_iter().flatten() {
            for c in r["checks"].as_array().into_iter().flatten() {
                if let (Some(l), Some(k)) = (c["loc"].as_str(), r["key"].as_str()) {
                    regions.insert(l.to_owned(), k.to_owned());
                }
            }
        }
        Ok(Self { built, prepared, regions })
    }

    /// Fill a room of `players` copies of the world with `options` (defaults for the rest)
    /// at random stream `seed`. The same arguments always give the same seed and spoiler.
    ///
    /// # Errors
    /// A bad instance, or a seed the engine cannot make, said for an author ([`explain`]).
    pub fn run(
        &self,
        options: &HashMap<String, i64>,
        players: u32,
        seed: u64,
    ) -> Result<Seed, String> {
        // Before any copy is made: a room has at most `ROOM_MAX_PLAYERS`, and a seed number
        // stays within what a JSON spoiler holds exactly.
        if players == 0 || usize::try_from(players).is_ok_and(|n| n > config::ROOM_MAX_PLAYERS) {
            return Err(format!("a room has 1 to {} players", config::ROOM_MAX_PLAYERS));
        }
        if seed > config::SEED_NUMBER_MAX {
            return Err(format!("a seed is a number from 0 to {}", config::SEED_NUMBER_MAX));
        }
        let instance = self.built.instance(options)?;
        let players: Vec<Player> = (1..=players)
            .map(|k| Player { name: format!("P{k}"), instance: instance.clone() })
            .collect();
        let name = format!("{}-{seed}", self.prepared.game());
        let (mut bundle, report) =
            generate_on(&self.prepared, &name, &players, seed).map_err(|e| explain(e, &players))?;
        let spoiler = self.spoiler(&bundle, options, seed, &report);
        let file = encode_on(&self.prepared, &mut bundle, SEED_MAX_BYTES)?;
        Ok(Seed { seed, name, file, spoiler, report })
    }

    /// The spoiler log of a filled `bundle`, as data.
    fn spoiler(
        &self,
        bundle: &SeedBundle,
        options: &HashMap<String, i64>,
        seed: u64,
        r: &FillReport,
    ) -> Value {
        let data = &self.prepared.data();
        let flip = |m: &BTreeMap<String, i64>| {
            m.iter().map(|(k, v)| (*v, k.clone())).collect::<HashMap<i64, String>>()
        };
        let (items, locs) = (flip(&data.item_name_to_id), flip(&data.location_name_to_id));
        let class = |f: u8| match f {
            f if f & 1 != 0 => "progression",
            f if f & 2 != 0 => "useful",
            f if f & 4 != 0 => "trap",
            _ => "filler",
        };
        let mut placements = Vec::new();
        for s in &bundle.slots {
            for p in &s.locations {
                let location =
                    locs.get(&p.location).cloned().unwrap_or_else(|| p.location.to_string());
                placements.push(json!({
                    "player": s.name,
                    "region": self.regions.get(&location).cloned().unwrap_or_default(),
                    "location": location,
                    "item": items.get(&p.item).cloned().unwrap_or_else(|| p.item.to_string()),
                    "receiver": bundle.slots.iter().find(|x| x.id == p.receiver).map_or_else(String::new, |x| x.name.clone()),
                    "class": class(p.flags),
                    "sphere": p.sphere,
                }));
            }
        }
        let shuffles: BTreeMap<&str, &Value> = bundle
            .slots
            .iter()
            .filter(|s| !s.slot_data["shuffles"].is_null())
            .map(|s| (s.name.as_str(), &s.slot_data["shuffles"]))
            .collect();
        let options: BTreeMap<String, String> =
            self.built.option_texts(options).into_iter().collect();
        json!({
            "seed": seed,
            "name": bundle.seed_name,
            "world": self.prepared.game(),
            "players": bundle.slots.len(),
            "options": options,
            "placements": placements,
            "shuffles": shuffles,
            "fill": {
                "attempts": r.attempts,
                "restarts": r.attempts.saturating_sub(1),
                "rounds_divided": r.divided,
                "items_moved": r.swaps,
                "tight_players": r.tight_players,
                "lowest_early_capacity": (r.capacity_milli != u32::MAX).then(|| f64::from(r.capacity_milli) / 1000.0),
            },
        })
    }
}

/// A seed the engine could not make, said for an author: what went wrong, and what to try.
/// `players` names whose item or goal it was when the room holds more than one.
#[must_use]
pub fn explain(e: GenerateError, players: &[Player]) -> String {
    let whose = |p: usize| match players.get(p) {
        Some(x) if players.len() > 1 => format!("{}'s ", x.name),
        _ => String::new(),
    };
    match e {
        GenerateError::Fill(FillError::Stuck { item, player, attempts }) => format!(
            "the fill could not place {}item `{item}` after {attempts} attempts: no reachable location was left for it\n  help: give `{item}` more places it can go early (checks with looser rules, or fewer items that need the same places), or lower its count",
            whose(player)
        ),
        GenerateError::Fill(FillError::Count { items, locations }) => format!(
            "{items} items to place and {locations} free checks\n  help: the two must match; `rando check` names the settings"
        ),
        GenerateError::Unbeatable { player } => format!(
            "the filled world leaves {}goal unreachable\n  help: the goal needs something the world never gives; check the goal's rule and that every item it needs is in the pool",
            if players.len() > 1 { whose(player) } else { "the ".to_owned() }
        ),
        GenerateError::Fill(FillError::Instance(m)) | GenerateError::Message(m) => m,
        GenerateError::Fill(FillError::Unstaged(what)) => {
            format!("the engine did not stage the world's shuffles: {what}")
        }
    }
}

/// The spoiler log for people: options, every placement by region and by item, the play order
/// (progression items by sphere), shuffle results, and how the fill went.
#[must_use]
pub fn spoiler_text(s: &Value) -> String {
    let mut out = String::new();
    let line = |out: &mut String, t: String| {
        out.push_str(&t);
        out.push('\n');
    };
    line(&mut out, format!("Seed {} ({})", s["seed"], s["name"].as_str().unwrap_or("")));
    line(
        &mut out,
        format!("World: {}, players: {}", s["world"].as_str().unwrap_or(""), s["players"]),
    );
    line(&mut out, "\nOptions".into());
    for (k, v) in s["options"].as_object().into_iter().flatten() {
        line(&mut out, format!("  {k}: {}", v.as_str().unwrap_or("")));
    }
    let all: Vec<&Value> = s["placements"].as_array().into_iter().flatten().collect();
    let str_of = |p: &Value, k: &str| p[k].as_str().unwrap_or("").to_owned();
    let multi = s["players"].as_u64().unwrap_or(1) > 1;
    let at = |p: &Value| {
        if multi {
            format!("{}: {}", str_of(p, "player"), str_of(p, "location"))
        } else {
            str_of(p, "location")
        }
    };
    let got = |p: &Value| {
        if multi {
            format!("{} for {}", str_of(p, "item"), str_of(p, "receiver"))
        } else {
            str_of(p, "item")
        }
    };
    // Players in room order (P2 before P10), from the order the placements are listed in.
    let mut order: Vec<String> = Vec::new();
    for p in &all {
        let name = str_of(p, "player");
        if !order.contains(&name) {
            order.push(name);
        }
    }
    let rank = |name: &str| order.iter().position(|x| x == name).unwrap_or(order.len());
    let mut by_region: BTreeMap<(usize, String, String), Vec<&Value>> = BTreeMap::new();
    let mut by_item: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut spheres: BTreeMap<u64, Vec<&Value>> = BTreeMap::new();
    for p in &all {
        let player = str_of(p, "player");
        by_region.entry((rank(&player), player, str_of(p, "region"))).or_default().push(p);
        by_item.entry(str_of(p, "item")).or_default().push(p);
        if p["class"] == "progression" {
            spheres.entry(p["sphere"].as_u64().unwrap_or(u64::MAX)).or_default().push(p);
        }
    }
    line(&mut out, format!("\nPlacements by region ({})", all.len()));
    for ((_, player, region), ps) in &by_region {
        line(&mut out, if multi { format!("  {player} {region}") } else { format!("  {region}") });
        for p in ps {
            line(&mut out, format!("    {}: {}", str_of(p, "location"), got(p)));
        }
    }
    line(&mut out, "\nPlacements by item".into());
    for (item, ps) in &by_item {
        let places: Vec<String> = ps.iter().map(|p| at(p)).collect();
        line(&mut out, format!("  {item} ({}): {}", ps.len(), places.join(", ")));
    }
    line(
        &mut out,
        "\nPlay order (progression items, by the sphere their location is first reachable in)"
            .into(),
    );
    for (k, ps) in &spheres {
        let label = if *k == u64::MAX { "?".to_owned() } else { k.to_string() };
        let list: Vec<String> = ps.iter().map(|p| format!("{} at {}", got(p), at(p))).collect();
        line(&mut out, format!("  sphere {label}: {}", list.join(", ")));
    }
    if let Some(sh) = s["shuffles"].as_object().filter(|m| !m.is_empty()) {
        line(&mut out, "\nShuffle results".into());
        // Per player, each shuffle by name: its slots and what each got, in key order.
        let pad = if multi { "    " } else { "  " };
        let mut sh: Vec<(&String, &Value)> = sh.iter().collect();
        sh.sort_by_key(|(player, _)| rank(player));
        for (player, tables) in sh {
            if multi {
                line(&mut out, format!("  {player}"));
            }
            for (name, table) in tables.as_object().into_iter().flatten() {
                line(&mut out, format!("{pad}{name}"));
                for (slot, got) in table.as_object().into_iter().flatten() {
                    let got = got.as_str().map_or_else(|| got.to_string(), str::to_owned);
                    line(&mut out, format!("{pad}  {slot} -> {got}"));
                }
            }
        }
    }
    let f = &s["fill"];
    line(&mut out, "\nFill".into());
    line(&mut out, format!("  attempts: {} (restarts {})", f["attempts"], f["restarts"]));
    line(
        &mut out,
        format!("  rounds divided: {}, items moved: {}", f["rounds_divided"], f["items_moved"]),
    );
    if f["tight_players"].as_u64().unwrap_or(0) > 0 {
        line(
            &mut out,
            format!(
                "  warning: {} players' worlds have less room early on than progression to place (lowest early capacity {}); the fill leaned on repairs",
                f["tight_players"], f["lowest_early_capacity"]
            ),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every engine refusal reads as words, never as a type's debug form.
    #[test]
    fn engine_refusals_read_as_words() {
        let one = |e| explain(e, &[]);
        assert_eq!(
            one(GenerateError::Fill(FillError::Instance("no item `x`".into()))),
            "no item `x`"
        );
        assert_eq!(one(GenerateError::Message("bad".into())), "bad");
        let u = one(GenerateError::Fill(FillError::Unstaged("shuffles")));
        assert!(u.starts_with("the engine did not stage") && !u.contains("Unstaged"), "{u}");
        let c = one(GenerateError::Fill(FillError::Count { items: 3, locations: 2 }));
        assert!(c.starts_with("3 items to place and 2 free checks"), "{c}");
        assert!(one(GenerateError::Unbeatable { player: 0 }).contains("the goal unreachable"));
    }
}

/// How `rando generate --check-seeds` went over a run of seeds.
#[derive(Debug, Clone, Default)]
pub struct SeedCheck {
    /// Seeds tried.
    pub tried: u64,
    /// Seeds that filled.
    pub filled: u64,
    /// Fill attempts over every filled seed (restarts are attempts past 1 each).
    pub attempts: u64,
    /// Restarts over every filled seed.
    pub restarts: u64,
    /// Fill time of each seed tried.
    pub times: Vec<Duration>,
    /// The first seed that failed, and its error.
    pub first_failure: Option<(u64, String)>,
}

impl Generator<'_> {
    /// Fill seeds `from` to `from + count - 1` and count what happened.
    ///
    /// # Errors
    /// No seeds, or a range that goes past the largest seed number.
    pub fn check_seeds(
        &self,
        options: &HashMap<String, i64>,
        players: u32,
        from: u64,
        count: u64,
    ) -> Result<SeedCheck, String> {
        let last = count
            .checked_sub(1)
            .and_then(|n| from.checked_add(n))
            .filter(|l| *l <= config::SEED_NUMBER_MAX)
            .ok_or_else(|| {
                format!(
                    "{count} seeds from {from} go past the largest seed number, {}",
                    config::SEED_NUMBER_MAX
                )
            })?;
        let mut c = SeedCheck::default();
        for seed in from..=last {
            let t = Instant::now();
            let r = self.run(options, players, seed);
            c.times.push(t.elapsed());
            c.tried += 1;
            match r {
                Ok(s) => {
                    c.filled += 1;
                    c.attempts += u64::from(s.report.attempts);
                    c.restarts += u64::from(s.report.attempts.saturating_sub(1));
                }
                Err(e) => {
                    c.first_failure.get_or_insert((seed, e));
                }
            }
        }
        Ok(c)
    }
}
