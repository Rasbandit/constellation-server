// Hint scenarios (docs/protocol-plan.md section 2.7) over a real room task. Who finds and who
// receives each item is read off the test bundle in `tests.rs` by eye: A's location 100 holds
// B's item 7 (progression), A's 101 holds A's own item 8, B's 100 holds A's item 7 (a trap).
// A hint costs each slot 1 point (10 percent of a world of 1 or 2, at least 1); a check earns 2.
use super::*;
use std::collections::BTreeMap;

fn hint_room(edit: impl FnOnce(&mut Value)) -> Arc<CompatRoom> {
    room_edit(|v| {
        v["games"][GAME]["item_name_groups"] = json!({"Numbers": ["Seven", "Eight"]});
        v["games"][GAME]["location_name_groups"] = json!({"Everywhere": ["L100", "L101"]});
        edit(v);
    })
}

fn scout(locations: &[i64], create_as_hint: &Value) -> Value {
    json!([{"cmd": "LocationScouts", "locations": locations, "create_as_hint": create_as_hint}])
}

fn say(text: &str) -> Value {
    json!([{"cmd": "Say", "text": text}])
}

fn hints_key(slot: u32) -> String {
    format!("_read_hints_0_{slot}")
}

async fn read_hints(s: &mut Session, slot: u32) -> Value {
    let key = hints_key(slot);
    let ps = send(s, &json!([{"cmd": "Get", "keys": [key]}])).await;
    ps[0]["keys"][&key].clone()
}

/// The `(finding, location, status, found)` of each hint print.
fn hint_prints(ps: &[Value]) -> Vec<(u64, i64, u64, bool)> {
    of(ps, "PrintJSON")
        .into_iter()
        .filter(|p| p["type"] == "Hint")
        .map(|p| {
            let status = p["data"].as_array().unwrap().last().unwrap()["hint_status"].as_u64();
            (
                p["item"]["player"].as_u64().unwrap(),
                p["item"]["location"].as_i64().unwrap(),
                status.unwrap(),
                p["found"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn command_results(ps: &[Value]) -> Vec<String> {
    of(ps, "PrintJSON")
        .into_iter()
        .filter(|p| p["type"] == "CommandResult")
        .map(|p| p["data"][0]["text"].as_str().unwrap().to_owned())
        .collect()
}

fn points(ps: &[Value]) -> Option<i64> {
    of(ps, "RoomUpdate").last().and_then(|p| p["hint_points"].as_i64())
}

#[tokio::test]
async fn a_scout_with_create_as_hint_stores_a_free_hint_and_tells_both_players() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut a).await;
    let ps = send(&mut a, &scout(&[100], &json!(1))).await;
    assert_eq!(ps[0]["cmd"], "LocationInfo");
    let to_a = pump(&mut a).await;
    let to_b = pump(&mut b).await;
    for got in [&to_a, &to_b] {
        assert_eq!(hint_prints(got), [(1, 100, 0, false)]);
        assert_eq!(points(got), Some(0), "a RoomUpdate to each side; scouts are free");
    }
    let print = of(&to_b, "PrintJSON")[0].clone();
    assert_eq!(
        print,
        json!({"cmd": "PrintJSON", "type": "Hint", "receiving": 2, "found": false,
            "item": {"class": "NetworkItem", "item": 7, "location": 100, "player": 1, "flags": 1},
            "data": [{"text": "Hint: "}, {"type": "player_id", "text": "2"}, {"text": "'s "},
                {"type": "item_id", "text": "7", "player": 2, "flags": 1}, {"text": " is at "},
                {"type": "location_id", "text": "100", "player": 1}, {"text": " in "},
                {"type": "player_id", "text": "1"}, {"text": "'s world"}, {"text": ". "},
                {"type": "hint_status", "text": "(unspecified)", "hint_status": 0}]})
    );
    let want = json!([{"class": "Hint", "receiving_player": 2, "finding_player": 1,
        "location": 100, "item": 7, "found": false, "entrance": "", "item_flags": 1, "status": 0}]);
    assert_eq!(read_hints(&mut b, 2).await, want);
    assert_eq!(read_hints(&mut b, 1).await, want, "the finder's list has it too");
}

#[tokio::test]
async fn a_scout_with_create_as_hint_two_announces_only_new_hints() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    send(&mut a, &scout(&[100], &json!(2))).await;
    assert_eq!(hint_prints(&pump(&mut a).await), [(1, 100, 0, false)]);
    send(&mut a, &scout(&[100, 101], &json!(2))).await;
    assert_eq!(hint_prints(&pump(&mut a).await), [(1, 101, 0, false)], "only the new one");
    // With 1 (or true, read as 1 like the reference's int()), known hints are announced again.
    send(&mut a, &scout(&[100], &json!(true))).await;
    assert_eq!(hint_prints(&pump(&mut a).await), [(1, 100, 0, false)]);
    // With 0, or without the field, nothing is created or announced.
    send(&mut a, &scout(&[100], &json!(0))).await;
    assert_eq!(hint_prints(&pump(&mut a).await), []);
    let mut b = joined(&room, "B").await;
    send(&mut b, &json!([{"cmd": "LocationScouts", "locations": [100]}])).await;
    assert_eq!(hint_prints(&pump(&mut b).await), []);
    assert_eq!(read_hints(&mut b, 2).await.as_array().unwrap().len(), 1, "only A's hint");
}

#[tokio::test]
async fn a_scout_of_a_found_location_still_stores_a_found_hint() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    check(&mut a, &[100]).await;
    pump(&mut a).await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    assert_eq!(hint_prints(&pump(&mut a).await), [(1, 100, 40, true)]);
    let got = read_hints(&mut a, 1).await;
    assert_eq!((got[0]["found"].clone(), got[0]["status"].clone()), (json!(true), json!(40)));
}

#[tokio::test]
async fn update_hint_by_the_receiver_changes_status_and_notifies_both_sides() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut a, &json!([{"cmd": "SetNotify", "keys": [hints_key(1)]}])).await;
    send(&mut b, &json!([{"cmd": "SetNotify", "keys": [hints_key(2)]}])).await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    pump(&mut a).await;
    pump(&mut b).await;
    let up = json!([{"cmd": "UpdateHint", "player": 1, "location": 100, "status": 20}]);
    assert_eq!(send(&mut b, &up).await, Vec::<Value>::new());
    for (s, slot) in [(&mut a, 1), (&mut b, 2)] {
        let got = pump(s).await;
        let replies = of(&got, "SetReply");
        assert_eq!(replies.len(), 1, "{got:?}");
        assert_eq!(replies[0]["key"], hints_key(slot));
        assert_eq!(replies[0]["value"][0]["status"], 20);
        assert_eq!(hint_prints(&got), [], "a status change prints nothing");
        assert_eq!(points(&got), None, "and changes no points");
    }
}

#[tokio::test]
async fn update_hint_by_anyone_else_is_refused_and_found_cannot_be_set() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    let up = |status: Value| json!([{"cmd": "UpdateHint", "player": 1, "location": 100, "status": status}]);
    let ps = send(&mut a, &up(json!(30))).await;
    assert_eq!(
        (ps[0]["cmd"].clone(), ps[0]["type"].clone()),
        (json!("InvalidPacket"), json!("arguments"))
    );
    assert!(ps[0]["text"].as_str().unwrap().contains("receiver"), "{ps:?}");
    for bad in [json!(40), json!(7), json!("x")] {
        let ps = send(&mut b, &up(bad.clone())).await;
        assert_eq!(ps[0]["cmd"], "InvalidPacket", "status {bad}");
    }
    for bad in [40, 7] {
        let ps = send(&mut b, &up(json!(bad))).await;
        assert!(ps[0]["text"].as_str().unwrap().contains("not a status"), "{ps:?}");
    }
    // An unknown hint and a null status are ignored silently, as in the reference.
    let unknown = json!([{"cmd": "UpdateHint", "player": 1, "location": 101, "status": 30}]);
    assert_eq!(send(&mut b, &unknown).await, Vec::<Value>::new());
    assert_eq!(send(&mut b, &up(Value::Null)).await, Vec::<Value>::new());
    assert_eq!(read_hints(&mut b, 2).await[0]["status"], 0);
}

#[tokio::test]
async fn hint_points_grow_with_checks_and_drop_by_the_cost_per_paid_hint() {
    let room = hint_room(|_| {});
    let mut b = joined(&room, "B").await;
    let got = {
        check(&mut b, &[100]).await;
        pump(&mut b).await
    };
    assert_eq!(points(&got), Some(2), "one check earns 2");
    send(&mut b, &say("!hint Seven")).await;
    let got = pump(&mut b).await;
    assert_eq!(hint_prints(&got), [(1, 100, 30, false)], "priority: not a trap");
    assert_eq!(points(&got), Some(1), "one paid hint costs 1");
    // The points survive a reconnect, and only the payer's points dropped.
    let mut again = Session::new(Arc::clone(&room));
    let ps = send(&mut again, &connect_packet("B", &json!(7))).await;
    assert_eq!(ps[0]["hint_points"], 1);
    let mut a = Session::new(Arc::clone(&room));
    let ps = send(&mut a, &connect_packet("A", &json!(7))).await;
    assert_eq!(ps[0]["hint_points"], 0);
}

/// A's world gets a second copy of B's item 7, at location 102 in sphere 0 (100 is in 1).
fn two_sevens(v: &mut Value) {
    v["slots"][0]["locations"][0]["sphere"] = json!(1);
    v["slots"][0]["locations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"location": 102, "item": 7, "receiver": 2, "flags": 1, "sphere": 0}));
    v["games"][GAME]["location_name_to_id"]["L102"] = json!(102);
}

#[tokio::test]
async fn bang_hint_pays_for_one_new_hint_per_command_when_hints_cost_points() {
    let room = hint_room(two_sevens);
    let mut b = joined(&room, "B").await;
    check(&mut b, &[100]).await;
    pump(&mut b).await;
    let ps = send(&mut b, &say("!hint Seven")).await;
    let got = pump(&mut b).await;
    assert_eq!(hint_prints(&got), [(1, 102, 30, false)], "the earlier sphere first");
    let text = command_results(&ps).join(" ");
    assert!(text.contains("run the command again"), "{text}");
    send(&mut b, &say("!hint Seven")).await;
    let got = pump(&mut b).await;
    assert_eq!(hint_prints(&got), [(1, 102, 30, false), (1, 100, 30, false)]);
    assert_eq!(points(&got), Some(0));
}

#[tokio::test]
async fn bang_hint_is_free_and_returns_all_when_hint_cost_is_zero() {
    let room = hint_room(|v| {
        two_sevens(v);
        v["server_options"]["hint_cost"] = json!(0);
    });
    let mut b = joined(&room, "B").await;
    let ps = send(&mut b, &say("!hint seven")).await;
    assert_eq!(command_results(&ps), Vec::<String>::new(), "{ps:?}");
    let got = pump(&mut b).await;
    assert_eq!(hint_prints(&got), [(1, 102, 30, false), (1, 100, 30, false)]);
    assert_eq!(points(&got), Some(0));
}

#[tokio::test]
async fn bang_hint_on_a_known_hint_shows_it_again_and_charges_nothing() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    pump(&mut b).await;
    let ps = send(&mut b, &say("!hint Seven")).await;
    assert!(command_results(&ps)[0].contains("no points"), "{ps:?}");
    let got = pump(&mut b).await;
    assert_eq!(hint_prints(&got), [(1, 100, 0, false)], "the stored hint, status unchanged");
    assert_eq!(points(&got), None, "nothing changed, no RoomUpdate");
}

#[tokio::test]
async fn bang_hint_prefers_the_earliest_sphere_then_another_players_world() {
    // B's own world gets item 7 too, at 101 in sphere 0 like A's 102.
    let room = hint_room(|v| {
        two_sevens(v);
        v["slots"][1]["locations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"location": 101, "item": 7, "receiver": 2, "flags": 0, "sphere": 0}));
    });
    let mut b = joined(&room, "B").await;
    check(&mut b, &[100]).await;
    pump(&mut b).await;
    send(&mut b, &say("!hint Seven")).await;
    assert_eq!(hint_prints(&pump(&mut b).await), [(1, 102, 30, false)]);
    send(&mut b, &say("!hint Seven")).await;
    let got = hint_prints(&pump(&mut b).await);
    assert!(got.contains(&(2, 101, 30, false)), "then the local one of the same sphere: {got:?}");
}

#[tokio::test]
async fn bang_hint_with_an_unknown_name_says_so_and_costs_nothing() {
    let room = hint_room(|_| {});
    let mut b = joined(&room, "B").await;
    let ps = send(&mut b, &say("!hint Sevn")).await;
    let text = command_results(&ps).join(" ");
    assert!(text.contains("Sevn") && text.contains("item group"), "{text}");
    let ps = send(&mut b, &say("!hint_location L999")).await;
    let text = command_results(&ps).join(" ");
    assert!(text.contains("location group named 'L999'"), "{text}");
    assert_eq!(hint_prints(&pump(&mut b).await), []);
}

#[tokio::test]
async fn bang_hint_on_a_blacklisted_item_says_it_is_not_hintable() {
    let room = hint_room(|v| v["games"][GAME]["hint_blacklist"] = json!(["Seven"]));
    let mut b = joined(&room, "B").await;
    let ps = send(&mut b, &say("!hint Seven")).await;
    assert!(command_results(&ps).join(" ").contains("cannot be hinted"), "{ps:?}");
    let ps = send(&mut b, &say("!hint 7")).await;
    assert!(command_results(&ps).join(" ").contains("'Seven' cannot be hinted"), "by id: {ps:?}");
    assert_eq!(hint_prints(&pump(&mut b).await), []);
}

#[tokio::test]
async fn bang_hint_with_too_few_points_says_what_it_costs() {
    let room = hint_room(|_| {});
    let mut b = joined(&room, "B").await;
    let ps = send(&mut b, &say("!hint Seven")).await;
    let text = command_results(&ps).join(" ");
    assert!(text.contains("0 points") && text.contains("need 1"), "{text}");
    assert_eq!(hint_prints(&pump(&mut b).await), []);
}

#[tokio::test]
async fn bang_commands_are_echoed_to_everyone_as_chat() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut a).await;
    send(&mut b, &say("!hint Seven")).await;
    let got = pump(&mut a).await;
    let chat = of(&got, "PrintJSON");
    assert_eq!(
        (chat[0]["type"].clone(), chat[0]["message"].clone()),
        (json!("Chat"), json!("!hint Seven"))
    );
}

#[tokio::test]
async fn a_check_marks_a_hint_found_and_notifies_hint_subscribers() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut b, &json!([{"cmd": "SetNotify", "keys": [hints_key(2)]}])).await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    let first = pump(&mut b).await;
    assert_eq!(of(&first, "SetReply").len(), 1, "a new hint is a change: {first:?}");
    check(&mut a, &[100]).await;
    let got = pump(&mut b).await;
    let replies = of(&got, "SetReply");
    assert_eq!(replies.len(), 1, "{got:?}");
    assert_eq!(
        (replies[0]["value"][0]["found"].clone(), replies[0]["value"][0]["status"].clone()),
        (json!(true), json!(40))
    );
    assert_eq!(replies[0].as_object().unwrap().len(), 3, "only cmd, key and value");
    // A check that touches no hint sends no SetReply.
    check(&mut a, &[101]).await;
    assert_eq!(of(&pump(&mut b).await, "SetReply").len(), 0);
}

#[tokio::test]
async fn hint_prints_reach_only_the_finder_and_the_receiver() {
    let room = hint_room(|v| {
        v["slots"].as_array_mut().unwrap().push(json!({"id": 3, "team": 0, "name": "C",
            "game": GAME, "kind": "player",
            "minimum_client_version": {"major": 0, "minor": 5, "build": 0},
            "locations": [], "slot_data": {}}));
        v["connect_names"]["C"] = json!([0, 3]);
    });
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    let mut c = joined(&room, "C").await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    assert_eq!(hint_prints(&pump(&mut a).await).len(), 1);
    assert_eq!(hint_prints(&pump(&mut b).await).len(), 1);
    let to_c = pump(&mut c).await;
    assert_eq!(hint_prints(&to_c).len(), 0);
    assert_eq!(points(&to_c), None, "no RoomUpdate for a slot the hint does not concern");
}

#[tokio::test]
async fn start_hints_from_the_seed_are_readable_on_first_connect() {
    let room = hint_room(|v| {
        v["start_hints"] = json!([{"finding": 2, "location": 100, "status": 30}]);
        v["slots"][1]["entrances"] = json!({"100": "Cave"});
    });
    let mut a = joined(&room, "A").await;
    let got = read_hints(&mut a, 1).await;
    assert_eq!(
        got,
        json!([{"class": "Hint", "receiving_player": 1, "finding_player": 2, "location": 100,
            "item": 7, "found": false, "entrance": "Cave", "item_flags": 4, "status": 30}])
    );
    // The print names the entrance.
    send(&mut a, &say("!hint")).await;
    let got = pump(&mut a).await;
    let print = of(&got, "PrintJSON").into_iter().find(|p| p["type"] == "Hint").unwrap().clone();
    let data = print["data"].as_array().unwrap();
    let at = data.iter().position(|p| p["type"] == "entrance_name").unwrap();
    assert_eq!(
        (data[at - 1]["text"].clone(), data[at]["text"].clone()),
        (json!(" at "), json!("Cave"))
    );
    assert_eq!(data[at + 2]["text"], "(priority)");
}

#[tokio::test]
async fn bang_hint_location_hints_a_location_of_your_own_world_by_name_or_group() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    check(&mut a, &[101]).await;
    pump(&mut a).await;
    send(&mut a, &say("!hint_location l100")).await;
    let got = pump(&mut a).await;
    assert_eq!(hint_prints(&got), [(1, 100, 0, false)], "unspecified, case-insensitive name");
    assert_eq!(points(&got), Some(1));
    // A group: 100 is known (shown free), 101 is checked (shown found, not kept).
    send(&mut a, &say("!HINT_LOCATION Everywhere")).await;
    let mut got = hint_prints(&pump(&mut a).await);
    got.sort_unstable();
    assert_eq!(got, [(1, 100, 0, false), (1, 101, 40, true)]);
    assert_eq!(read_hints(&mut a, 1).await.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn bang_hint_by_item_group_or_id_finds_every_member() {
    let room = hint_room(|v| v["server_options"]["hint_cost"] = json!(0));
    let mut a = joined(&room, "A").await;
    send(&mut a, &say("!hint Numbers")).await;
    let mut got = hint_prints(&pump(&mut a).await);
    got.sort_unstable();
    // A receives 7 from B's 100 (a trap: avoid) and its own 8 from 101.
    assert_eq!(got, [(1, 101, 30, false), (2, 100, 20, false)]);
    let mut b = joined(&room, "B").await;
    send(&mut b, &say("!hint 7")).await;
    assert_eq!(hint_prints(&pump(&mut b).await), [(1, 100, 30, false)]);
}

#[tokio::test]
async fn bare_bang_hint_lists_your_hints_and_your_points() {
    let room = hint_room(|_| {});
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    pump(&mut a).await;
    pump(&mut b).await;
    let ps = send(&mut b, &say("!hint")).await;
    let text = command_results(&ps).join(" ");
    assert!(text.contains("costs 1") && text.contains("have 0"), "{text}");
    assert_eq!(hint_prints(&pump(&mut b).await), [(1, 100, 0, false)]);
    assert_eq!(hint_prints(&pump(&mut a).await), [], "only to the asker");
}

#[tokio::test]
async fn hint_creating_packets_are_rate_limited_per_slot() {
    let room =
        room_with_limits(&Limits { hint_burst: 1, hint_refill_milli_per_sec: 0, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    // Review finding: a limited scout lost its LocationInfo, which shop clients wait for. Now
    // it is answered, and only the hint is refused.
    let ps = send(&mut a, &scout(&[101], &json!(2))).await;
    assert_eq!(ps[0]["cmd"], "LocationInfo");
    assert_eq!(ps[0]["locations"][0]["location"], 101);
    assert_eq!(ps[1]["cmd"], "InvalidPacket");
    assert_eq!(ps.len(), 2);
    assert_eq!(read_hints(&mut a, 1).await.as_array().unwrap().len(), 1, "no hint for 101");
    // A plain scout is not limited.
    assert_eq!(send(&mut a, &scout(&[101], &json!(0))).await[0]["cmd"], "LocationInfo");
    let up = json!([{"cmd": "UpdateHint", "player": 1, "location": 100, "status": 30}]);
    let mut b = joined(&room, "B").await;
    assert_eq!(send(&mut b, &up).await, Vec::<Value>::new());
    assert_eq!(send(&mut b, &up).await[0]["cmd"], "InvalidPacket");
}

/// The base bundle (no edits) with the given limits.
fn room_with_limits(limits: &Limits) -> Arc<CompatRoom> {
    spawn_with(bundle(&Value::Null), None, limits)
}

#[tokio::test]
async fn hint_prints_put_the_slots_own_finds_first_and_found_ones_first_within() {
    // B's world gets its own item 7 at 101 too.
    let room = hint_room(|v| {
        v["slots"][1]["locations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"location": 101, "item": 7, "receiver": 2, "flags": 0}));
    });
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut a, &scout(&[100], &json!(1))).await;
    send(&mut b, &scout(&[100, 101], &json!(1))).await;
    check(&mut b, &[101]).await;
    pump(&mut b).await;
    send(&mut b, &say("!hint")).await;
    assert_eq!(
        hint_prints(&pump(&mut b).await),
        [(2, 101, 40, true), (2, 100, 0, false), (1, 100, 0, false)]
    );
}

#[tokio::test]
async fn bang_hint_says_when_nothing_matches_and_when_more_cost_too_much() {
    let room = hint_room(|v| {
        two_sevens(v);
        v["server_options"]["location_check_points"] = json!(1);
    });
    let mut b = joined(&room, "B").await;
    // Only A receives Eight: with too few points B hears what a hint costs, with enough that
    // nothing matches.
    let ps = send(&mut b, &say("!hint Eight")).await;
    assert!(command_results(&ps)[0].contains("need 1"), "{ps:?}");
    check(&mut b, &[100]).await;
    pump(&mut b).await;
    let ps = send(&mut b, &say("!hint Eight")).await;
    assert!(
        command_results(&ps)[0].contains("Nothing in this multiworld matches 'Eight'"),
        "{ps:?}"
    );
    // One point buys one of the two sevens; the other is left for lack of points.
    let ps = send(&mut b, &say("!hint Seven")).await;
    let text = command_results(&ps).join(" ");
    assert!(text.contains("There may be more, but you have 0 points and need 1"), "{text}");
}

#[test]
fn a_hint_can_only_be_in_the_finders_and_the_receivers_list() {
    let lists = BTreeMap::from([(1, 0), (2, 0), (3, 0)]);
    let of = |f, r| session::lists_of_hint(&lists, f, r).collect::<Vec<_>>();
    assert_eq!(of(1, Some(2)), [1, 2]);
    assert_eq!(of(1, Some(1)), [1], "a slot's own item: one list");
    assert_eq!(of(4, Some(2)), [2], "a list the session does not keep is skipped");
    assert_eq!(of(4, None), Vec::<u32>::new());
}

#[test]
fn every_hint_status_has_its_name() {
    let names: Vec<String> =
        [0, 10, 20, 30, 40, 7].into_iter().map(|s| wire::TextPart::hint_status(s).text).collect();
    assert_eq!(
        names,
        ["(unspecified)", "(no priority)", "(avoid)", "(priority)", "(found)", "(unknown)"]
    );
}

/// Review finding: a group resolved each member by a scan of every name, so `!hint_location
/// Everywhere` (Archipelago gives every world that group) cost names x members compares on the
/// edge task. Now names and groups are indexed once per package; 40,000 members take
/// milliseconds where the scan took 14.7 s, so the 3 s budget has about a thousandfold margin
/// (timed on purpose: the cost is the edge's own CPU).
#[tokio::test]
async fn a_group_of_every_location_resolves_within_a_small_budget() {
    let n = 40_000;
    let mut v: Value = bundle_json(&Value::Null);
    let locs: Vec<Value> = (1000..1000 + n)
        .map(|l| json!({"location": l, "item": 7, "receiver": 2, "flags": 0}))
        .collect();
    v["slots"][0]["locations"] = json!(locs);
    let names: Vec<String> = (1000..1000 + n).map(|l| format!("L{l}")).collect();
    for (l, name) in (1000..1000 + n).zip(&names) {
        v["games"][GAME]["location_name_to_id"][name] = json!(l);
    }
    v["games"][GAME]["location_name_groups"] = json!({"Everywhere": names});
    let room = spawn_with(seed(&v), None, &Limits::DESIGN);
    let mut a = joined(&room, "A").await;
    pump(&mut a).await;
    let started = std::time::Instant::now();
    let ps = send(&mut a, &say("!hint_location Everywhere")).await;
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(3), "took {took:?}");
    // A hint costs 10 percent of 40,000 locations; nothing is checked yet.
    assert_eq!(command_results(&ps), ["You have 0 points and need 4000 for a hint."]);
}

/// Review finding: every check scanned every watched hint list of the session, and each list
/// was a full copy, so a watcher of big lists paid lists x hints per checked location. Now a
/// check touches at most two list entries (the finder's and the receiver's), the room is asked
/// whether the location is hinted at all, and a session keeps one digest per list. Counted,
/// not timed: on the old code the entries touched were every hint of every kept list.
#[tokio::test]
async fn a_check_touches_at_most_two_hint_lists() {
    let n: u64 = 300;
    let room = big_room(1000 + i64::try_from(n).unwrap(), &Limits::DESIGN);
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    let all: Vec<i64> = (1000..1000 + i64::try_from(n).unwrap()).collect();
    send(&mut b, &json!([{"cmd": "SetNotify", "keys": [hints_key(1), hints_key(2)]}])).await;
    send(&mut a, &scout(&all, &json!(1))).await;
    pump(&mut b).await;
    let before = b.hint_work();
    check(&mut a, &all).await;
    let got = pump(&mut b).await;
    let (touched, reread) = (b.hint_work().0 - before.0, b.hint_work().1 - before.1);
    // Each check touches the finder's watched list, and at most the receiver's too.
    assert!((n..=2 * n).contains(&touched), "{touched} list entries for {n} checks");
    assert!(reread <= 2 * n, "{reread} lists reread for {n} checks");
    let replies = of(&got, "SetReply");
    assert!(!replies.is_empty() && replies.len() as u64 <= reread, "{} replies", replies.len());
    for key in [hints_key(1), hints_key(2)] {
        let last = replies.iter().rev().find(|r| r["key"] == key.as_str()).unwrap();
        let hints = last["value"].as_array().unwrap();
        assert_eq!(hints.len() as u64, n);
        assert!(hints.iter().all(|h| h["found"] == true), "converged");
    }
}
