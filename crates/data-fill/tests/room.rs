//! Several players filled together on the small fixture world (`room_world.json`): player 1
//! plays it as is, player 2 with a harder hall door, player 3 with a goal that stops at the
//! hall and one location fewer.

use data_fill::room::{self, Given, Seat, fill_room_spheres, locked_placed, validate_room};
use data_fill::spheres::room_spheres;
use data_fill::{Instance, Plan, World, fill};
use serde_json::Value;
use spec_rng::SpecRng;

const FIXTURE: &str = include_str!("fixtures/room_world.json");

/// The fixture's world and one instance per player.
fn load() -> (Value, Vec<Instance>) {
    let doc: Value = serde_json::from_str(FIXTURE).unwrap();
    let insts = room::instances(doc["instances"].as_array().unwrap()).unwrap();
    (doc["manifest"].clone(), insts)
}

/// Compiled worlds, plans and progression flags for `n` players (the fixture's three, repeated).
struct Room {
    worlds: Vec<World>,
    plans: Vec<Plan>,
    progression: Vec<Vec<bool>>,
}

impl Room {
    fn new(n: usize, workers: usize) -> Self {
        let (m, insts) = load();
        let insts: Vec<&Instance> = (0..n).map(|p| &insts[p % insts.len()]).collect();
        let manifests: Vec<&Value> = vec![&m; n];
        let worlds = room::worlds(&manifests, workers).unwrap();
        let pairs: Vec<(&World, &Instance)> = worlds.iter().zip(insts.iter().copied()).collect();
        let plans = room::plans(&pairs, workers).unwrap();
        let progression = worlds.iter().zip(&insts).map(|(w, i)| i.progression(w)).collect();
        Room { worlds, plans, progression }
    }

    fn seats(&self) -> Vec<Seat<'_>> {
        (0..self.plans.len())
            .map(|p| Seat {
                w: &self.worlds[p],
                plan: &self.plans[p],
                progression: &self.progression[p],
            })
            .collect()
    }
}

fn rng(seed: u64) -> SpecRng {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&seed.to_le_bytes());
    SpecRng::from_seed(b)
}

/// The placements as `player:location=owner:item` lines, the form a seed file would hold.
fn lines(seats: &[Seat<'_>], placed: &[Vec<Option<Given>>]) -> String {
    let mut out = String::new();
    for (p, row) in placed.iter().enumerate() {
        for (l, g) in row.iter().enumerate() {
            if let Some(g) = g {
                let (w, o) = (seats[p].w, seats[usize::from(g.to)].w);
                out += &format!(
                    "{}:{}={}:{}\n",
                    p + 1,
                    w.loc_key(u32::try_from(l).unwrap()),
                    g.to + 1,
                    o.item_key(g.item)
                );
            }
        }
    }
    out
}

/// Placements written as `(player, location, owner, item)` with 1-based players.
fn hand(seats: &[Seat<'_>], list: &[(u16, &str, u16, &str)]) -> Vec<Vec<Option<Given>>> {
    let mut placed = locked_placed(seats);
    for (p, l, o, i) in list {
        let w = seats[usize::from(p - 1)].w;
        let item = seats[usize::from(o - 1)].w.item(i).unwrap();
        let slot = &mut placed[usize::from(p - 1)][w.loc(l).unwrap() as usize];
        assert!(slot.is_none(), "{p}:{l} placed twice");
        *slot = Some(Given { to: o - 1, item });
    }
    placed
}

/// Player 1's key sits in player 3's first room, player 3's in player 2's, player 2's in
/// player 1's, and player 2's lamp in player 3's: every door of every player is opened by an
/// item lying in another world. Everything else stays at home.
const CHAIN: &[(u16, &str, u16, &str)] = &[
    (1, "l1", 2, "key_a"),
    (1, "l2", 1, "coin"),
    (1, "l3", 1, "key_b"),
    (1, "l4", 1, "coin"),
    (1, "l5", 1, "lamp"),
    (1, "l6", 1, "coin"),
    (2, "l1", 3, "key_a"),
    (2, "l2", 2, "coin"),
    (2, "l3", 2, "key_b"),
    (2, "l4", 2, "coin"),
    (2, "l5", 2, "coin"),
    (2, "l6", 3, "coin"),
    (3, "l1", 1, "key_a"),
    (3, "l2", 2, "lamp"),
    (3, "l3", 3, "key_b"),
    (3, "l4", 3, "lamp"),
    (3, "l6", 3, "coin"),
];

#[test]
fn an_item_in_another_world_opens_the_door_for_a_third_player() {
    let r = Room::new(3, 1);
    let seats = r.seats();
    let placed = hand(&seats, CHAIN);
    let (beatable, reached) = validate_room(&seats, &placed);
    assert_eq!(beatable, [true, true, true], "linked, every player finishes");
    assert_eq!(reached, [6, 6, 5]);
    // Hand calculation: round 1 is the free locations of every world; each key found there
    // opens a hall; the hall holds the other keys, then the vaults; player 1's last location
    // needs the lamp that lies in its vault.
    let sp = room_spheres(&seats, &placed);
    let key = |s: &Vec<(u16, u32)>| -> Vec<String> {
        s.iter()
            .map(|(p, l)| format!("{}:{}", p + 1, seats[usize::from(*p)].w.loc_key(*l)))
            .collect()
    };
    let got: Vec<Vec<String>> = sp.spheres.iter().map(key).collect();
    let want: &[&[&str]] = &[
        &["1:l1", "1:l2", "2:l1", "2:l2", "3:l1", "3:l2"],
        &["1:l3", "1:l4", "2:l3", "2:l4", "2:l6", "3:l3", "3:l4"],
        &["1:l5", "2:l5", "3:l6"],
        &["1:l6"],
    ];
    assert_eq!(got, want);
    assert!(sp.all_beatable() && sp.unreachable.is_empty());

    // Without the links (each player holding only what lies in their own world) nobody can
    // finish: this is what the room's linking adds.
    for (p, s) in seats.iter().enumerate() {
        let mut own = locked_placed(&seats);
        for (l, g) in placed[p].iter().enumerate() {
            own[p][l] = g.filter(|g| usize::from(g.to) == p);
        }
        let alone = [Seat { ..*s }];
        let row = vec![own[p].iter().map(|g| g.map(|g| Given { to: 0, ..g })).collect::<Vec<_>>()];
        assert!(!validate_room(&alone, &row).0[0], "player {} alone", p + 1);
    }
}

#[test]
fn an_item_behind_its_own_door_strands_only_its_owner() {
    let r = Room::new(3, 1);
    let seats = r.seats();
    // Player 1's key moves into player 1's vault (behind the door it opens) and the lamp that
    // was there goes to player 3's first room: player 1 is stuck, the others do not need it.
    let mut list = CHAIN.to_vec();
    list.iter_mut().find(|x| (x.0, x.1) == (1, "l5")).unwrap().3 = "key_a";
    list.iter_mut().find(|x| (x.0, x.1) == (3, "l1")).unwrap().3 = "lamp";
    let placed = hand(&seats, &list);
    let (beatable, _) = validate_room(&seats, &placed);
    assert_eq!(beatable, [false, true, true]);
}

#[test]
fn three_players_fill_beatable_rooms_with_items_crossing_both_ways() {
    let r = Room::new(3, 1);
    let seats = r.seats();
    // crossed[a][b]: a progression item of player a seen in player b's world.
    let mut crossed = [[false; 3]; 3];
    for seed in 1u64..=60 {
        let (f, sp) = fill_room_spheres(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS)
            .unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        assert!(f.all_beatable(), "seed {seed}");
        assert!(sp.all_beatable(), "seed {seed}");
        assert!(sp.unreachable.is_empty(), "seed {seed}: every placed item is reachable");
        // The report read off the fill's states is a fresh sweep's.
        assert_eq!(sp, room_spheres(&seats, &f.placed), "seed {seed}");
        assert_eq!((f.beatable.clone(), f.reached.clone()), validate_room(&seats, &f.placed));
        // Everything is placed, each item once, and nothing leaves its owner's pool.
        let mut per_owner = [0usize; 3];
        for (p, row) in f.placed.iter().enumerate() {
            for (l, g) in row.iter().enumerate() {
                let g = g.unwrap_or_else(|| {
                    assert!(
                        !seats[p].plan.included.contains(&u32::try_from(l).unwrap()),
                        "an existing location is empty"
                    );
                    Given { to: 9, item: 0 }
                });
                if g.to < 3 {
                    per_owner[usize::from(g.to)] += 1;
                    assert!(room::permits(&seats, g, u16::try_from(p).unwrap(), l as u32));
                    if seats[usize::from(g.to)].progression[usize::from(g.item)] {
                        crossed[usize::from(g.to)][p] = true;
                    }
                }
            }
        }
        assert_eq!(per_owner, [6, 6, 5], "seed {seed}");
    }
    for a in 0..3 {
        for b in 0..3 {
            if a != b {
                assert!(
                    crossed[a][b],
                    "no seed put player {}'s progression in world {}",
                    a + 1,
                    b + 1
                );
            }
        }
    }
}

#[test]
fn the_same_seed_gives_the_same_bytes_and_other_seeds_differ() {
    let r = Room::new(3, 1);
    let seats = r.seats();
    let run = |seed| {
        let f = room::fill_room(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS).unwrap();
        lines(&seats, &f.placed)
    };
    assert_eq!(run(7), run(7));
    assert_ne!(run(7), run(8));
}

#[test]
fn the_result_is_the_same_with_one_worker_and_with_many() {
    // Seven players, so every worker count splits them differently.
    let outputs: Vec<Vec<String>> = [1usize, 2, 3, 7, 16]
        .iter()
        .map(|workers| {
            let r = Room::new(7, *workers);
            let seats = r.seats();
            (1u64..=5)
                .map(|seed| {
                    let f =
                        room::fill_room(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS).unwrap();
                    assert!(f.all_beatable());
                    lines(&seats, &f.placed)
                })
                .collect()
        })
        .collect();
    assert!(outputs.windows(2).all(|p| p[0] == p[1]));
}

#[test]
fn a_room_of_one_fills_exactly_like_a_single_player() {
    let r = Room::new(1, 1);
    let seats = r.seats();
    for seed in 1u64..=40 {
        let one =
            fill(&r.worlds[0], &r.plans[0], &mut rng(seed), config::FILL_MAX_ATTEMPTS).unwrap();
        let many = room::fill_room(&seats, &mut rng(seed), config::FILL_MAX_ATTEMPTS).unwrap();
        let flat: Vec<Option<u16>> = many.placed[0].iter().map(|g| g.map(|g| g.item)).collect();
        assert_eq!(one.placed, flat, "seed {seed}");
    }
}

#[test]
fn a_room_needs_players_and_matching_counts() {
    let r = Room::new(3, 1);
    let seats = r.seats();
    let e = room::fill_room(&[], &mut rng(1), 1).unwrap_err();
    assert!(matches!(e, data_fill::FillError::Instance(_)), "{e:?}");
    // Drop a player's seat's locations: more items than places.
    let (m, mut insts) = load();
    insts[2].locations.pop();
    let w = World::load(&m).unwrap();
    let plan = Plan::new(&w, &insts[2]).unwrap();
    let prog = insts[2].progression(&w);
    let broken = [seats[0], seats[1], Seat { w: &w, plan: &plan, progression: &prog }];
    let e = room::fill_room(&broken, &mut rng(1), 1).unwrap_err();
    assert!(matches!(e, data_fill::FillError::Count { .. }), "{e:?}");
}

#[test]
fn an_unknown_key_names_the_player() {
    let (m, mut insts) = load();
    insts[1].pool.push("nope".to_owned());
    let w = World::load(&m).unwrap();
    let pairs = [(&w, &insts[0]), (&w, &insts[1]), (&w, &insts[2])];
    for workers in [1, 3] {
        match room::plans(&pairs, workers) {
            Err(data_fill::FillError::Instance(msg)) => {
                assert!(msg.starts_with("player 2:"), "{msg}")
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn shared_data_is_send_and_sync_and_a_state_can_move_to_a_task() {
    fn shared<T: Send + Sync>() {}
    fn owned<T: Send>() {}
    shared::<World>();
    shared::<Plan>();
    shared::<Instance>();
    owned::<data_fill::Reach<'static>>();
}

#[test]
fn the_seed_output_names_a_slot_and_a_receiver_per_placement() {
    let r = Room::new(3, 1);
    let seats = r.seats();
    let placed = hand(&seats, CHAIN);
    let sp = room_spheres(&seats, &placed);
    let out = room::placements(&seats, &placed, Some(&sp));
    assert_eq!(out.iter().map(Vec::len).collect::<Vec<_>>(), [6, 6, 5], "one list per slot");
    // Slot 1, location l1 (id 1) holds player 2's key_a (id 1): receiver 2, progression,
    // found in the first round.
    let first = out[0][0];
    assert_eq!((first.location, first.item, first.receiver, first.flags), (1, 1, 2, 1));
    assert_eq!(first.sphere, Some(0));
    // Slot 2, location l6 (id 6) holds player 3's coin (id 4): receiver 3, not progression,
    // found in the second round (the lamp found in round 1 opens it).
    let coin = out[1][5];
    assert_eq!((coin.location, coin.item, coin.receiver, coin.flags), (6, 4, 3, 0));
    assert_eq!(coin.sphere, Some(1));
    // Without spheres none is recorded.
    assert!(room::placements(&seats, &placed, None).iter().flatten().all(|p| p.sphere.is_none()));
    // Progression of each owner lying in other worlds, over all they own.
    assert_eq!(room::cross_world_share(&seats, &placed), [(1, 3), (2, 3), (1, 3)]);
}
