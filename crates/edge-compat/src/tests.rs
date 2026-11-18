// Session tests over a real room task and MemLogStore. Expected packets come from the protocol
// document's field tables and from the hand-written bundle below (who receives what is read
// off it by eye).
use super::*;
use log::MemLogStore;
use proto::RoomId;
use room_core::RoomState;
use serde_json::{Value, json};
use tokio::sync::mpsc::error::TryRecvError;

const GAME: &str = "G";

/// Slot 1 (A): location 100 -> B's item 7 (progression), 101 -> A's own item 8.
/// Slot 2 (B): location 100 -> A's item 7 (trap).
fn bundle(password: &Value) -> Arc<SeedBundle> {
    let v = json!({
        "format": 1, "seed_name": "seed-1",
        "generator_version": {"major": 0, "minor": 6, "build": 8},
        "server_options": {"password": password, "hint_cost": 10, "location_check_points": 2,
            "release_mode": "auto", "collect_mode": "auto", "remaining_mode": "goal"},
        "slots": [
            {"id": 1, "team": 0, "name": "A", "game": GAME, "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 5, "build": 0},
             "locations": [{"location": 100, "item": 7, "receiver": 2, "flags": 1},
                           {"location": 101, "item": 8, "receiver": 1, "flags": 0}],
             "slot_data": {"answer": 42}},
            {"id": 2, "team": 0, "name": "B", "game": GAME, "kind": "player",
             "minimum_client_version": {"major": 0, "minor": 5, "build": 0},
             "locations": [{"location": 100, "item": 7, "receiver": 1, "flags": 4}],
             "slot_data": {}}
        ],
        "connect_names": {"A": [0, 1], "B": [0, 2]},
        "games": {GAME: {"item_name_to_id": {"Seven": 7, "Eight": 8},
                         "location_name_to_id": {"L100": 100, "L101": 101}, "checksum": "ck"}}
    });
    Arc::new(SeedBundle::from_json(&v.to_string()).unwrap())
}

fn room_with(password: &Value, override_pw: Option<&str>) -> Arc<CompatRoom> {
    let b = bundle(password);
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let state = RoomState::new(b.slot_nos(), b.placement()).unwrap();
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, Limits::DESIGN);
    Arc::new(CompatRoom::new(b, h, override_pw.map(str::to_owned), Limits::DESIGN).unwrap())
}

fn room() -> Arc<CompatRoom> {
    room_with(&Value::Null, None)
}

fn connect_packet(name: &str, handling: &Value) -> Value {
    json!([{"cmd": "Connect", "password": null, "game": GAME, "name": name, "uuid": "u",
            "version": {"major": 0, "minor": 6, "build": 8, "class": "Version"},
            "items_handling": handling, "tags": ["AP"], "slot_data": true}])
}

/// Every packet in the frames, flattened.
fn packets(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .flat_map(|f| match serde_json::from_str::<Value>(f).unwrap() {
            Value::Array(v) => v,
            other => panic!("frame is not a list: {other}"),
        })
        .collect()
}

async fn send(s: &mut Session, v: &Value) -> Vec<Value> {
    packets(&s.on_text(&v.to_string()).await.unwrap())
}

/// Everything the room has published to this session so far, resyncing from the cursor when
/// the room dropped the queue (as the socket loop does).
async fn pump(s: &mut Session) -> Vec<Value> {
    let mut frames = Vec::new();
    let mut resyncs = 0..100;
    loop {
        let next = match s.live() {
            Some(rx) => rx.try_recv(),
            None => return Vec::new(),
        };
        match next {
            Ok(p) => frames.extend(s.on_published(&p).unwrap()),
            Err(TryRecvError::Empty) => return packets(&frames),
            Err(TryRecvError::Disconnected) => {
                // A resync that does not resubscribe leaves the queue closed: fail, do not spin.
                assert!(resyncs.next().is_some(), "resync did not give the session a live queue");
                frames.extend(s.resync().await.unwrap());
            }
        }
    }
}

fn of<'a>(ps: &'a [Value], cmd: &str) -> Vec<&'a Value> {
    ps.iter().filter(|p| p["cmd"] == cmd).collect()
}

async fn joined(room: &Arc<CompatRoom>, name: &str) -> Session {
    let mut s = Session::new(Arc::clone(room));
    let ps = send(&mut s, &connect_packet(name, &json!(7))).await;
    assert_eq!(ps[0]["cmd"], "Connected", "{ps:?}");
    pump(&mut s).await;
    s
}

async fn check(s: &mut Session, locations: &[i64]) -> Vec<Value> {
    send(s, &json!([{"cmd": "LocationChecks", "locations": locations}])).await
}

#[tokio::test]
async fn the_first_frame_is_room_info_with_checksums_and_no_password() {
    let s = Session::new(room());
    let ps = packets(&[s.hello()]);
    let info = &ps[0];
    assert_eq!(info["cmd"], "RoomInfo");
    assert_eq!(info["password"], false);
    assert_eq!(info["datapackage_checksums"], json!({GAME: "ck"}));
    assert_eq!(info["games"], json!([GAME]));
    assert_eq!(info["seed_name"], "seed-1");
    assert_eq!(info["version"]["class"], "Version");
    assert_eq!(
        info["generator_version"],
        json!({"major": 0, "minor": 6, "build": 8, "class": "Version"})
    );
    assert_eq!(info["permissions"], json!({"release": 0, "collect": 0, "remaining": 0}));
    assert!(info["time"].as_f64().unwrap() > 1.7e9);
}

#[tokio::test]
async fn connected_lists_the_slot_players_and_every_location_as_missing() {
    let mut s = Session::new(room());
    let ps = send(&mut s, &connect_packet("A", &json!(7))).await;
    assert_eq!(ps.len(), 1, "no items yet, so no ReceivedItems: {ps:?}");
    let c = &ps[0];
    assert_eq!((c["team"].clone(), c["slot"].clone()), (json!(0), json!(1)));
    assert_eq!(c["missing_locations"], json!([100, 101]));
    assert_eq!(c["checked_locations"], json!([]));
    assert_eq!(c["slot_data"], json!({"answer": 42}));
    assert_eq!(c["hint_points"], 0);
    assert_eq!(
        c["slot_info"]["2"],
        json!({"name": "B", "game": GAME, "type": 1, "group_members": []})
    );
    assert_eq!(c["players"][1], json!({"team": 0, "slot": 2, "alias": "B", "name": "B"}));
}

#[tokio::test]
async fn slot_data_is_left_out_when_the_client_does_not_ask() {
    let mut s = Session::new(room());
    let mut p = connect_packet("A", &json!(7));
    p[0]["slot_data"] = json!(false);
    let ps = send(&mut s, &p).await;
    assert!(ps[0].get("slot_data").is_none());
}

#[tokio::test]
async fn each_bad_connect_is_refused_with_its_error_and_the_socket_can_try_again() {
    let mut s = Session::new(room());
    let cases = [
        (connect_packet("Nobody", &json!(7)), "InvalidSlot"),
        (connect_packet("A", &json!(6)), "InvalidItemsHandling"),
        (connect_packet("A", &json!("7")), "InvalidItemsHandling"),
    ];
    for (p, want) in cases {
        let ps = send(&mut s, &p).await;
        assert_eq!(ps, [json!({"cmd": "ConnectionRefused", "errors": [want]})], "{p}");
    }
    let mut wrong_game = connect_packet("A", &json!(7));
    wrong_game[0]["game"] = json!("Other");
    assert_eq!(send(&mut s, &wrong_game).await[0]["errors"], json!(["InvalidGame"]));
    let mut old = connect_packet("A", &json!(7));
    old[0]["version"] = json!({"major": 0, "minor": 4, "build": 9});
    assert_eq!(send(&mut s, &old).await[0]["errors"], json!(["IncompatibleVersion"]));
    assert_eq!(send(&mut s, &connect_packet("A", &json!(7))).await[0]["cmd"], "Connected");
}

#[tokio::test]
async fn a_wrong_or_missing_password_is_refused_and_the_right_one_connects() {
    for room in [room_with(&json!("pw"), None), room_with(&Value::Null, Some("pw"))] {
        let mut s = Session::new(room);
        assert_eq!(packets(&[s.hello()])[0]["password"], true);
        let ps = send(&mut s, &connect_packet("A", &json!(7))).await;
        assert_eq!(ps[0]["errors"], json!(["InvalidPassword"]));
        let mut p = connect_packet("Nobody", &json!(7));
        p[0]["password"] = json!("bad");
        assert_eq!(send(&mut s, &p).await[0]["errors"], json!(["InvalidPassword", "InvalidSlot"]));
        let mut p = connect_packet("A", &json!(7));
        p[0]["password"] = json!("pw");
        assert_eq!(send(&mut s, &p).await[0]["cmd"], "Connected");
    }
}

#[tokio::test]
async fn a_tracker_may_connect_without_a_game_but_may_not_check() {
    let mut s = Session::new(room());
    let mut p = connect_packet("A", &json!(0));
    p[0]["game"] = json!("");
    p[0]["tags"] = json!(["Tracker"]);
    assert_eq!(send(&mut s, &p).await[0]["cmd"], "Connected");
    let ps = check(&mut s, &[100]).await;
    assert_eq!(ps[0]["cmd"], "InvalidPacket");
    assert_eq!(ps[0]["original_cmd"], "LocationChecks");
}

#[tokio::test]
async fn hostile_frames_get_invalid_packet_and_the_session_survives() {
    let mut s = Session::new(room());
    let cases = ["not json", "{\"cmd\":\"Sync\"}", "", "[", "[1]", "[{\"cmd\":5}]", "[{}]"];
    for text in cases {
        let ps = packets(&s.on_text(text).await.unwrap());
        assert_eq!(ps.len(), 1, "{text:?}");
        assert_eq!(
            (ps[0]["cmd"].clone(), ps[0]["type"].clone()),
            (json!("InvalidPacket"), json!("cmd")),
            "{text:?}"
        );
        assert_eq!(ps[0]["original_cmd"], Value::Null, "{text:?}");
    }
    let ps = send(&mut s, &json!([{"cmd": "Teleport"}])).await;
    assert_eq!(ps[0]["original_cmd"], "Teleport");
    let ps = send(&mut s, &json!([{"cmd": "Connect", "name": 5}])).await;
    assert_eq!(
        (ps[0]["type"].clone(), ps[0]["original_cmd"].clone()),
        (json!("arguments"), json!("Connect"))
    );
    let ps = send(&mut s, &json!([{"cmd": "Sync"}])).await;
    assert_eq!(ps[0]["text"], "Sync: connect first");
    assert_eq!(send(&mut s, &connect_packet("A", &json!(7))).await[0]["cmd"], "Connected");
    let ps = send(&mut s, &json!([{"cmd": "LocationChecks", "locations": "all"}])).await;
    assert_eq!(
        (ps[0]["type"].clone(), ps[0]["original_cmd"].clone()),
        (json!("arguments"), json!("LocationChecks"))
    );
}

#[tokio::test]
async fn a_check_sends_the_item_to_its_receiver_with_the_next_index() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut a).await;
    assert_eq!(check(&mut a, &[100]).await, Vec::<Value>::new());
    let to_b = pump(&mut b).await;
    let items = of(&to_b, "ReceivedItems");
    assert_eq!(items.len(), 1, "{to_b:?}");
    assert_eq!(
        *items[0],
        json!({"cmd": "ReceivedItems", "index": 0,
        "items": [{"item": 7, "location": 100, "player": 1, "flags": 1}]})
    );
    let to_a = pump(&mut a).await;
    assert_eq!(of(&to_a, "ReceivedItems"), Vec::<&Value>::new());
    assert_eq!(
        *of(&to_a, "RoomUpdate")[0],
        json!({"cmd": "RoomUpdate", "checked_locations": [100], "hint_points": 2})
    );
    let send = of(&to_a, "PrintJSON").into_iter().find(|p| p["type"] == "ItemSend").unwrap();
    assert_eq!(send["receiving"], 2);
    assert_eq!(send["item"], json!({"item": 7, "location": 100, "player": 1, "flags": 1}));
}

#[tokio::test]
async fn an_own_item_comes_back_only_to_a_client_that_wants_own_items() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut remote_only = Session::new(Arc::clone(&room));
    send(&mut remote_only, &connect_packet("A", &json!(1))).await;
    pump(&mut remote_only).await;
    check(&mut a, &[101]).await;
    let got = pump(&mut a).await;
    assert_eq!(
        of(&got, "ReceivedItems")[0]["items"],
        json!([{"item": 8, "location": 101, "player": 1, "flags": 0}])
    );
    assert_eq!(of(&pump(&mut remote_only).await, "ReceivedItems"), Vec::<&Value>::new());
}

#[tokio::test]
async fn repeated_and_unknown_checks_deliver_nothing() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    check(&mut a, &[100, 100]).await;
    assert_eq!(of(&pump(&mut b).await, "ReceivedItems").len(), 1);
    check(&mut a, &[100, 999, -1]).await;
    assert_eq!(pump(&mut b).await, Vec::<Value>::new());
    assert_eq!(pump(&mut a).await.iter().filter(|p| p["cmd"] == "RoomUpdate").count(), 1);
}

#[tokio::test]
async fn a_reconnect_and_a_sync_resend_every_item_from_index_zero() {
    let room = room();
    let mut a = joined(&room, "A").await;
    check(&mut a, &[100, 101]).await;
    let mut b = joined(&room, "B").await;
    check(&mut b, &[100]).await;
    let mut again = Session::new(Arc::clone(&room));
    let ps = send(&mut again, &connect_packet("A", &json!(7))).await;
    assert_eq!(ps[0]["checked_locations"], json!([100, 101]));
    assert_eq!(ps[0]["missing_locations"], json!([]));
    assert_eq!(ps[0]["hint_points"], 4);
    assert_eq!(
        ps[1],
        json!({"cmd": "ReceivedItems", "index": 0, "items": [
        {"item": 8, "location": 101, "player": 1, "flags": 0},
        {"item": 7, "location": 100, "player": 2, "flags": 4}]})
    );
    pump(&mut again).await;
    let ps = send(&mut again, &json!([{"cmd": "Sync"}])).await;
    assert_eq!(ps, [ps[0].clone()]);
    assert_eq!(ps[0]["index"], 0);
    assert_eq!(ps[0]["items"].as_array().unwrap().len(), 2);
    // A later item continues after the two already sent.
    let room2 = room_with(&Value::Null, None);
    let mut a2 = joined(&room2, "A").await;
    let mut b2 = joined(&room2, "B").await;
    check(&mut b2, &[100]).await;
    check(&mut a2, &[101]).await;
    let idx: Vec<_> =
        of(&pump(&mut a2).await, "ReceivedItems").iter().map(|p| p["index"].clone()).collect();
    assert_eq!(idx, [json!(0), json!(1)]);
}

#[tokio::test]
async fn a_goal_is_recorded_once_announced_and_readable() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    assert_eq!(
        send(&mut a, &json!([{"cmd": "StatusUpdate", "status": 20}])).await,
        Vec::<Value>::new()
    );
    send(&mut a, &json!([{"cmd": "StatusUpdate", "status": 30}])).await;
    send(&mut a, &json!([{"cmd": "StatusUpdate", "status": 30}])).await;
    let goals: Vec<_> = pump(&mut b).await.into_iter().filter(|p| p["type"] == "Goal").collect();
    assert_eq!(goals.len(), 1);
    assert_eq!((goals[0]["team"].clone(), goals[0]["slot"].clone()), (json!(0), json!(1)));
    let get = json!([{"cmd": "Get", "keys": ["_read_client_status_0_1", "_read_client_status_0_2", "free", "_read_slot_data_1"], "tag": 9}]);
    let ps = send(&mut b, &get).await;
    assert_eq!(
        ps,
        [json!({"cmd": "Retrieved", "tag": 9, "keys": {
        "_read_client_status_0_1": 30, "_read_client_status_0_2": 0, "free": null,
        "_read_slot_data_1": {"answer": 42}}})]
    );
    let mut late = Session::new(Arc::clone(&room));
    send(&mut late, &connect_packet("B", &json!(7))).await;
    let ps = send(&mut late, &json!([{"cmd": "Get", "keys": ["_read_client_status_0_1"]}])).await;
    assert_eq!(ps[0]["keys"]["_read_client_status_0_1"], 30, "the goal survives in the log");
}

#[tokio::test]
async fn a_bounce_reaches_only_its_targets() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut a).await;
    send(&mut a, &json!([{"cmd": "Bounce", "slots": [2], "data": {"x": 1}}])).await;
    assert_eq!(pump(&mut b).await, [json!({"cmd": "Bounced", "slots": [2], "data": {"x": 1}})]);
    assert_eq!(pump(&mut a).await, Vec::<Value>::new());
    // Legacy with only teams names nobody; "or" with the default team reaches everyone.
    send(&mut a, &json!([{"cmd": "Bounce", "teams": [0]}])).await;
    assert_eq!(pump(&mut b).await, Vec::<Value>::new());
    send(&mut a, &json!([{"cmd": "Bounce", "operator": "or", "tags": ["DeathLink"]}])).await;
    assert_eq!(pump(&mut b).await.len(), 1);
    send(&mut a, &json!([{"cmd": "Bounce", "operator": "and", "games": [GAME], "tags": []}])).await;
    assert_eq!(pump(&mut b).await, Vec::<Value>::new());
    let ps = send(&mut a, &json!([{"cmd": "Bounce", "operator": "xor"}])).await;
    assert_eq!(ps[0]["type"], "arguments");
}

#[tokio::test]
async fn chat_reaches_everyone_and_no_text_clients_skip_prints() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut quiet = Session::new(Arc::clone(&room));
    let mut p = connect_packet("B", &json!(7));
    p[0]["tags"] = json!(["AP", "NoText"]);
    send(&mut quiet, &p).await;
    pump(&mut a).await;
    send(&mut a, &json!([{"cmd": "Say", "text": "hello"}])).await;
    let chat = pump(&mut a).await;
    assert_eq!(chat[0]["type"], "Chat");
    assert_eq!(chat[0]["message"], "hello");
    assert_eq!(pump(&mut quiet).await, Vec::<Value>::new());
    let ps = send(&mut a, &json!([{"cmd": "Say", "text": "!hint x"}])).await;
    assert_eq!(ps[0]["type"], "CommandResult");
}

#[tokio::test]
async fn scouts_answer_with_the_receiver_of_each_item() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let ps = send(&mut a, &json!([{"cmd": "LocationScouts", "locations": [100, 101]}])).await;
    assert_eq!(
        ps,
        [json!({"cmd": "LocationInfo", "locations": [
        {"item": 7, "location": 100, "player": 2, "flags": 1},
        {"item": 8, "location": 101, "player": 1, "flags": 0}]})]
    );
}

#[tokio::test]
async fn the_data_package_is_sent_whole_or_per_game() {
    let mut s = Session::new(room());
    let all = send(&mut s, &json!([{"cmd": "GetDataPackage"}])).await;
    assert_eq!(all[0]["data"]["games"][GAME]["checksum"], "ck");
    assert_eq!(all[0]["data"]["games"][GAME]["item_name_to_id"]["Seven"], 7);
    let none = send(&mut s, &json!([{"cmd": "GetDataPackage", "games": ["Other"]}])).await;
    assert_eq!(none, [json!({"cmd": "DataPackage", "data": {"games": {}}})]);
}

#[tokio::test]
async fn connect_update_changes_items_handling_and_resends_items() {
    let room = room();
    let mut a = Session::new(Arc::clone(&room));
    send(&mut a, &connect_packet("A", &json!(1))).await;
    check(&mut a, &[101]).await;
    assert_eq!(of(&pump(&mut a).await, "ReceivedItems"), Vec::<&Value>::new());
    let ps = send(&mut a, &json!([{"cmd": "ConnectUpdate", "items_handling": 3}])).await;
    assert_eq!(ps[0]["index"], 0);
    assert_eq!(ps[0]["items"][0]["item"], 8);
    let ps = send(&mut a, &json!([{"cmd": "ConnectUpdate", "items_handling": 4}])).await;
    assert_eq!(ps[0]["type"], "arguments");
}

#[tokio::test]
async fn unsupported_commands_say_so() {
    let mut a = joined(&room(), "A").await;
    for cmd in ["Set", "SetNotify", "CreateHints", "UpdateHint"] {
        let ps = send(&mut a, &json!([{"cmd": cmd}])).await;
        assert_eq!(ps[0]["original_cmd"], cmd);
        assert!(ps[0]["text"].as_str().unwrap().contains("not supported"));
    }
}

#[test]
fn a_check_cid_is_the_location_and_distinct_per_location() {
    assert_eq!(check_cid(0), Cid(0));
    assert_eq!(check_cid(1), Cid(1));
    assert_ne!(check_cid(-1), check_cid(1));
    assert_eq!(check_cid(-1), Cid(u64::MAX));
}

/// A room where slot A has locations `1000..end` that all send item 7 to B.
fn big_room(end: i64, limits: Limits) -> Arc<CompatRoom> {
    big_room_to(end, 2, limits)
}

/// As [`big_room`], every item going to `receiver`.
fn big_room_to(end: i64, receiver: u32, limits: Limits) -> Arc<CompatRoom> {
    let mut v: Value = serde_json::to_value(&*bundle(&Value::Null)).unwrap();
    let locs: Vec<Value> = (1000..end)
        .map(|l| json!({"location": l, "item": 7, "receiver": receiver, "flags": 0}))
        .collect();
    v["slots"][0]["locations"] = json!(locs);
    for l in 1000..end {
        v["games"][GAME]["location_name_to_id"][format!("L{l}")] = json!(l);
    }
    let b = Arc::new(SeedBundle::from_json(&v.to_string()).unwrap());
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let state = RoomState::new(b.slot_nos(), b.placement()).unwrap();
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, limits);
    Arc::new(CompatRoom::new(b, h, None, limits).unwrap())
}

fn indexes(ps: &[Value]) -> Vec<u64> {
    of(ps, "ReceivedItems")
        .iter()
        .flat_map(|p| {
            let first = p["index"].as_u64().unwrap();
            let n = u64::try_from(p["items"].as_array().unwrap().len()).unwrap();
            first..first.checked_add(n).unwrap()
        })
        .collect()
}

#[tokio::test]
async fn two_hundred_checks_in_one_packet_reach_a_bystander_that_stays_connected() {
    let room = big_room(1200, Limits::DESIGN);
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut a).await;
    let all: Vec<i64> = (1000..1200).collect();
    check(&mut a, &all).await;
    let got = pump(&mut b).await;
    assert_eq!(indexes(&got), (0..200).collect::<Vec<_>>());
    assert!(b.live().is_some(), "still connected");
    let mine = pump(&mut a).await;
    assert_eq!(of(&mine, "RoomUpdate")[0]["checked_locations"].as_array().unwrap().len(), 200);
}

#[tokio::test]
async fn a_slow_session_resyncs_from_its_cursor_and_gets_every_item_once_in_order() {
    // A one-entry queue: B falls behind after the first batch and the room drops its queue.
    let room = big_room(1030, Limits { session_queue: 1, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    for chunk in (1000..1030).collect::<Vec<i64>>().chunks(7) {
        check(&mut a, chunk).await;
    }
    let got = pump(&mut b).await;
    assert_eq!(indexes(&got), (0..30).collect::<Vec<_>>());
    let locations: Vec<i64> = of(&got, "ReceivedItems")
        .iter()
        .flat_map(|p| {
            p["items"].as_array().unwrap().iter().map(|i| i["location"].as_i64().unwrap())
        })
        .collect();
    assert_eq!(locations, (1000..1030).collect::<Vec<_>>());
    check(&mut a, &[1000]).await;
    assert_eq!(indexes(&pump(&mut b).await), Vec::<u64>::new(), "nothing twice after the resync");
}

fn limited(limits: Limits) -> Arc<CompatRoom> {
    let b = bundle(&Value::Null);
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let state = RoomState::new(b.slot_nos(), b.placement()).unwrap();
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, limits);
    Arc::new(CompatRoom::new(b, h, None, limits).unwrap())
}

/// A burst of 2 that never refills, so the test does not depend on the clock.
const TIGHT: Limits = Limits {
    fanout_burst: 2,
    fanout_refill_milli_per_sec: 0,
    datapackage_burst: 1,
    datapackage_refill_milli_per_sec: 0,
    chat_max_bytes: 8,
    bounce_max_bytes: 64,
    ..Limits::DESIGN
};

#[tokio::test]
async fn chat_and_bounce_share_a_rate_limit_and_the_excess_is_refused() {
    let room = limited(TIGHT);
    let mut a = joined(&room, "A").await;
    assert_eq!(send(&mut a, &json!([{"cmd": "Say", "text": "hi"}])).await, Vec::<Value>::new());
    assert_eq!(send(&mut a, &json!([{"cmd": "Bounce", "slots": [2]}])).await, Vec::<Value>::new());
    for p in [json!([{"cmd": "Say", "text": "hi"}]), json!([{"cmd": "Bounce", "slots": [2]}])] {
        let ps = send(&mut a, &p).await;
        assert_eq!(
            (ps[0]["cmd"].clone(), ps[0]["type"].clone()),
            (json!("InvalidPacket"), json!("arguments"))
        );
        assert!(ps[0]["text"].as_str().unwrap().contains("rate"), "{ps:?}");
    }
}

#[tokio::test]
async fn oversized_chat_and_bounce_are_refused_before_they_cost_a_token() {
    let room = limited(TIGHT);
    let mut a = joined(&room, "A").await;
    let ps = send(&mut a, &json!([{"cmd": "Say", "text": "123456789"}])).await;
    assert!(ps[0]["text"].as_str().unwrap().contains("too long"), "{ps:?}");
    let ps = send(&mut a, &json!([{"cmd": "Bounce", "data": {"x": "y".repeat(64)}}])).await;
    assert!(ps[0]["text"].as_str().unwrap().contains("too large"), "{ps:?}");
    // Exactly at the cap is fine, and the refusals took no tokens.
    assert_eq!(
        send(&mut a, &json!([{"cmd": "Say", "text": "12345678"}])).await,
        Vec::<Value>::new()
    );
    assert_eq!(send(&mut a, &json!([{"cmd": "Say", "text": "1"}])).await, Vec::<Value>::new());
}

#[tokio::test]
async fn get_data_package_is_rate_limited() {
    let mut s = Session::new(limited(TIGHT));
    assert_eq!(send(&mut s, &json!([{"cmd": "GetDataPackage"}])).await[0]["cmd"], "DataPackage");
    let ps = send(&mut s, &json!([{"cmd": "GetDataPackage"}])).await;
    assert_eq!(ps[0]["original_cmd"], "GetDataPackage");
    assert!(ps[0]["text"].as_str().unwrap().contains("rate"));
}

#[test]
fn a_bucket_refills_at_its_rate_and_never_above_its_burst() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let mut b = limit::Bucket::new(2, 1000, t0);
    assert!(b.take(t0) && b.take(t0) && !b.take(t0));
    assert!(!b.take(t0 + Duration::from_millis(999)));
    assert!(b.take(t0 + Duration::from_millis(1000)));
    let later = t0 + Duration::from_secs(3600);
    assert!(b.take(later) && b.take(later) && !b.take(later), "capped at the burst");
    let mut never = limit::Bucket::new(0, 1000, t0);
    assert!(!never.take(later));
}

/// The test bundle with an edit applied.
fn room_edit(edit: impl FnOnce(&mut Value)) -> Arc<CompatRoom> {
    let mut v: Value = serde_json::to_value(&*bundle(&Value::Null)).unwrap();
    edit(&mut v);
    let b = Arc::new(SeedBundle::from_json(&v.to_string()).unwrap());
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let state = RoomState::new(b.slot_nos(), b.placement()).unwrap();
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, Limits::DESIGN);
    Arc::new(CompatRoom::new(b, h, None, Limits::DESIGN).unwrap())
}

fn tracker_packet(name: &str, version: (u32, u32, u32)) -> Value {
    let mut p = connect_packet(name, &json!(0));
    p[0]["game"] = json!("");
    p[0]["tags"] = json!(["Tracker"]);
    p[0]["version"] = json!({"major": version.0, "minor": version.1, "build": version.2});
    p
}

#[tokio::test]
async fn a_tracker_cannot_declare_a_goal() {
    let mut t = Session::new(room());
    send(&mut t, &tracker_packet("A", (0, 6, 8))).await;
    let ps = send(&mut t, &json!([{"cmd": "StatusUpdate", "status": 30}])).await;
    assert_eq!(
        (ps[0]["cmd"].clone(), ps[0]["original_cmd"].clone()),
        (json!("InvalidPacket"), json!("StatusUpdate"))
    );
    let ps = send(&mut t, &json!([{"cmd": "Get", "keys": ["_read_client_status_0_1"]}])).await;
    assert_eq!(ps[0]["keys"]["_read_client_status_0_1"], 0);
}

#[tokio::test]
async fn trackers_meet_the_global_minimum_version_and_games_meet_their_slots() {
    let room = room_edit(|v| {
        v["slots"][0]["minimum_client_version"] = json!({"major": 0, "minor": 6, "build": 0});
    });
    let mut s = Session::new(Arc::clone(&room));
    assert_eq!(send(&mut s, &tracker_packet("A", (0, 5, 5))).await[0]["cmd"], "Connected");
    let mut s = Session::new(Arc::clone(&room));
    let ps = send(&mut s, &tracker_packet("A", (0, 4, 9))).await;
    assert_eq!(ps[0]["errors"], json!(["IncompatibleVersion"]));
    let mut s = Session::new(room);
    let mut p = connect_packet("A", &json!(7));
    p[0]["version"] = json!({"major": 0, "minor": 5, "build": 5});
    assert_eq!(send(&mut s, &p).await[0]["errors"], json!(["IncompatibleVersion"]));
}

#[tokio::test]
async fn no_empty_received_items_and_none_for_clients_that_take_no_items() {
    let room = room();
    let mut a = joined(&room, "A").await;
    assert_eq!(send(&mut a, &json!([{"cmd": "Sync"}])).await, Vec::<Value>::new(), "nothing yet");
    let mut none = Session::new(Arc::clone(&room));
    send(&mut none, &connect_packet("A", &json!(0))).await;
    check(&mut a, &[101]).await;
    pump(&mut none).await;
    assert_eq!(send(&mut none, &json!([{"cmd": "Sync"}])).await, Vec::<Value>::new());
    let ps =
        send(&mut none, &json!([{"cmd": "ConnectUpdate", "items_handling": 0, "tags": ["X"]}]))
            .await;
    assert_eq!(ps, Vec::<Value>::new());
}

#[tokio::test]
async fn a_bad_connect_update_changes_nothing_not_even_the_tags() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let ps =
        send(&mut a, &json!([{"cmd": "ConnectUpdate", "items_handling": 6, "tags": ["NoText"]}]))
            .await;
    assert_eq!(ps[0]["type"], "arguments");
    send(&mut a, &json!([{"cmd": "Say", "text": "still here"}])).await;
    assert_eq!(pump(&mut a).await[0]["type"], "Chat", "NoText was not applied");
}

#[tokio::test]
async fn items_handling_bits_above_the_three_flags_are_ignored() {
    let mut s = Session::new(room());
    assert_eq!(send(&mut s, &connect_packet("A", &json!(15))).await[0]["cmd"], "Connected");
    let mut s = Session::new(room());
    assert_eq!(send(&mut s, &connect_packet("A", &json!(8))).await[0]["cmd"], "Connected");
    for bad in [json!(6), json!(-2), json!(1.5), json!("7"), Value::Null] {
        let mut s = Session::new(room());
        assert_eq!(
            send(&mut s, &connect_packet("A", &bad)).await[0]["errors"],
            json!(["InvalidItemsHandling"]),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn a_self_found_item_reads_found_their() {
    let room = room();
    let mut a = joined(&room, "A").await;
    check(&mut a, &[101]).await;
    let print = pump(&mut a).await.into_iter().find(|p| p["type"] == "ItemSend").unwrap();
    let texts: Vec<&str> =
        print["data"].as_array().unwrap().iter().filter_map(|p| p["text"].as_str()).collect();
    assert_eq!(texts, ["1", " found their ", "8", " (", "101", ")"]);
}

#[tokio::test]
async fn scouts_answer_in_request_order_and_refuse_unknown_ids() {
    let mut a = joined(&room(), "A").await;
    let ps = send(&mut a, &json!([{"cmd": "LocationScouts", "locations": [101, 100, 101]}])).await;
    let locs: Vec<_> =
        ps[0]["locations"].as_array().unwrap().iter().map(|l| l["location"].clone()).collect();
    assert_eq!(locs, [json!(101), json!(100), json!(101)]);
    let ps = send(&mut a, &json!([{"cmd": "LocationScouts", "locations": [100, 5]}])).await;
    assert_eq!((ps.len(), ps[0]["type"].clone()), (1, json!("arguments")));
}

#[tokio::test]
async fn hints_read_as_empty_and_race_mode_as_the_rooms_value() {
    let mut a = Session::new(room_edit(|v| {
        v["race_mode"] = json!(1);
    }));
    send(&mut a, &connect_packet("A", &json!(7))).await;
    let ps = send(&mut a, &json!([{"cmd": "Get", "keys": ["_read_hints_0_1", "_read_race_mode"]}]))
        .await;
    assert_eq!(ps[0]["keys"], json!({"_read_hints_0_1": [], "_read_race_mode": 1}));
}

#[tokio::test]
async fn reconnecting_to_another_slot_announces_part_then_join_and_the_same_slot_nothing() {
    let room = room();
    let mut watcher = joined(&room, "A").await;
    let mut s = joined(&room, "B").await;
    pump(&mut watcher).await;
    send(&mut s, &connect_packet("B", &json!(7))).await;
    assert_eq!(pump(&mut watcher).await, Vec::<Value>::new(), "same slot: no prints");
    send(&mut s, &connect_packet("A", &json!(7))).await;
    let seen: Vec<_> =
        pump(&mut watcher).await.iter().map(|p| (p["type"].clone(), p["slot"].clone())).collect();
    assert_eq!(seen, [(json!("Part"), json!(2)), (json!("Join"), json!(1))]);
}

fn peer(n: u8) -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, n))
}

#[tokio::test]
async fn reconnecting_does_not_refill_the_chat_bucket_of_a_slot() {
    let room = limited(TIGHT);
    let mut a = joined(&room, "A").await;
    for _ in 0..2 {
        assert_eq!(send(&mut a, &json!([{"cmd": "Say", "text": "hi"}])).await, Vec::<Value>::new());
    }
    drop(a);
    let mut again = Session::with_peer(Arc::clone(&room), peer(9));
    send(&mut again, &connect_packet("A", &json!(7))).await;
    let ps = send(&mut again, &json!([{"cmd": "Say", "text": "hi"}])).await;
    assert!(
        ps[0]["text"].as_str().unwrap().contains("rate"),
        "the slot's bucket is still empty: {ps:?}"
    );
    let mut b = joined(&room, "B").await;
    assert_eq!(
        send(&mut b, &json!([{"cmd": "Say", "text": "hi"}])).await,
        Vec::<Value>::new(),
        "another slot is not limited"
    );
}

#[tokio::test]
async fn the_data_package_limit_follows_the_client_address_across_connections() {
    let room = limited(TIGHT);
    let dp = json!([{"cmd": "GetDataPackage"}]);
    let mut first = Session::with_peer(Arc::clone(&room), peer(1));
    assert_eq!(send(&mut first, &dp).await[0]["cmd"], "DataPackage");
    let mut second = Session::with_peer(Arc::clone(&room), peer(1));
    assert_eq!(send(&mut second, &dp).await[0]["cmd"], "InvalidPacket", "same address, new socket");
    let mut other = Session::with_peer(Arc::clone(&room), peer(2));
    assert_eq!(send(&mut other, &dp).await[0]["cmd"], "DataPackage");
}

#[tokio::test]
async fn connect_is_rate_limited_per_address() {
    let room =
        limited(Limits { connect_burst: 2, connect_refill_milli_per_sec: 0, ..Limits::DESIGN });
    for _ in 0..2 {
        let mut s = Session::with_peer(Arc::clone(&room), peer(3));
        assert_eq!(send(&mut s, &connect_packet("A", &json!(7))).await[0]["cmd"], "Connected");
    }
    let mut s = Session::with_peer(Arc::clone(&room), peer(3));
    let ps = send(&mut s, &connect_packet("A", &json!(7))).await;
    assert_eq!(
        (ps[0]["cmd"].clone(), ps[0]["original_cmd"].clone()),
        (json!("InvalidPacket"), json!("Connect"))
    );
    let mut elsewhere = Session::with_peer(Arc::clone(&room), peer(4));
    assert_eq!(send(&mut elsewhere, &connect_packet("A", &json!(7))).await[0]["cmd"], "Connected");
}

#[tokio::test]
async fn many_reconnects_in_a_quiet_room_keep_the_subscriber_count_bounded() {
    let room = limited(Limits { connect_burst: 1000, ..Limits::DESIGN });
    let mut s = Session::new(Arc::clone(&room));
    for _ in 0..50 {
        // The same slot again announces nothing, so nothing is published to prune on.
        send(&mut s, &connect_packet("A", &json!(7))).await;
    }
    assert_eq!(room.room.subscribers().await.unwrap(), 1);
}

#[tokio::test]
async fn debug_output_never_shows_the_room_password() {
    let room = room_with(&json!("hunter2"), None);
    let text = format!("{room:?}");
    assert!(!text.contains("hunter2") && text.contains("redacted"), "{text}");
}

#[test]
fn a_full_rate_table_forgets_idle_keys_and_refuses_while_all_are_limited() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let table = RateTable::new(1, 1000, 1);
    assert!(table.take(1_u32, t0));
    assert!(!table.take(2_u32, t0), "key 1 is still limited, so no room for key 2");
    let later = t0 + Duration::from_secs(5);
    assert!(table.take(2_u32, later), "key 1 refilled, so it was forgotten");
}

#[tokio::test]
async fn a_slow_checker_resyncs_its_own_checks_and_items_with_nothing_missed_or_doubled() {
    // A one-entry queue, and A checks without reading: A itself falls behind. The first event A
    // misses is one of its own checks, so a resync that starts one event late loses a check.
    let room = big_room_to(1030, 1, Limits { session_queue: 1, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    for chunk in (1000..1030).collect::<Vec<i64>>().chunks(7) {
        check(&mut a, chunk).await;
    }
    let got = pump(&mut a).await;
    let checked: Vec<i64> = of(&got, "RoomUpdate")
        .iter()
        .flat_map(|p| {
            p["checked_locations"].as_array().unwrap().iter().map(|l| l.as_i64().unwrap())
        })
        .collect();
    assert_eq!(checked, (1000..1030).collect::<Vec<_>>(), "every check once, in order");
    let items: Vec<i64> = of(&got, "ReceivedItems")
        .iter()
        .flat_map(|p| {
            p["items"].as_array().unwrap().iter().map(|i| i["location"].as_i64().unwrap())
        })
        .collect();
    assert_eq!(items, (1000..1030).collect::<Vec<_>>(), "every item once, in order");
    assert_eq!(indexes(&got), (0..30).collect::<Vec<_>>());
}

#[tokio::test]
async fn items_handling_is_read_as_bits_like_the_reference_so_minus_one_and_true_are_flag_sets() {
    let room = room();
    let mut a = joined(&room, "A").await;
    // -1 has every bit set: all three flags, so own items too.
    let mut all = Session::new(Arc::clone(&room));
    assert_eq!(send(&mut all, &connect_packet("A", &json!(-1))).await[0]["cmd"], "Connected");
    // true is 1: items from other worlds only.
    let mut remote = Session::new(Arc::clone(&room));
    assert_eq!(send(&mut remote, &connect_packet("A", &json!(true))).await[0]["cmd"], "Connected");
    let mut none = Session::new(Arc::clone(&room));
    assert_eq!(send(&mut none, &connect_packet("A", &json!(false))).await[0]["cmd"], "Connected");
    pump(&mut all).await;
    pump(&mut remote).await;
    pump(&mut none).await;
    check(&mut a, &[101]).await; // A's own item
    assert_eq!(of(&pump(&mut all).await, "ReceivedItems").len(), 1);
    assert_eq!(of(&pump(&mut remote).await, "ReceivedItems").len(), 0);
    assert_eq!(of(&pump(&mut none).await, "ReceivedItems").len(), 0);
}

// The tests below pin behaviour the nightly mutation run showed was unasserted. Packet shapes
// follow the protocol document's PrintJSON table (which fields each type carries) and its
// JSONMessagePart table (a `player_id` part carries the slot id as a decimal string).
// In the Join, Part and Goal prints below, `type`, `team`, `slot` and `tags` (and that `data`
// is a list of parts naming the player with a `player_id` part) come from the protocol doc.
// The text wording (" joined the room.", " left the room.", " reached their goal.") is ours,
// not Archipelago's; clients show `data` as given, so wording is free.

#[test]
fn polling_a_bucket_often_does_not_stop_it_refilling() {
    use std::time::{Duration, Instant};
    // 500 milli-tokens a second: one token every 2 s (hand calculation: 1000 / 500).
    let t0 = Instant::now();
    let mut b = limit::Bucket::new(1, 500, t0);
    assert!(b.take(t0));
    // Asked every millisecond, each call alone gains less than one milli-token.
    let mut refilled_at = None;
    for ms in 1..=2000 {
        if b.take(t0 + Duration::from_millis(ms)) {
            refilled_at = Some(ms);
            break;
        }
    }
    assert_eq!(refilled_at, Some(2000), "small gains must accumulate, not be dropped");
}

#[test]
fn a_known_key_in_a_full_rate_table_keeps_its_own_tokens() {
    use std::time::Instant;
    let t0 = Instant::now();
    let table = RateTable::new(2, 0, 1);
    assert!(table.take(1_u32, t0));
    assert!(table.take(1_u32, t0), "the table is full, but key 1 is already in it");
    assert!(!table.take(1_u32, t0), "and its burst of 2 is spent");
}

#[tokio::test]
async fn a_session_is_joined_only_after_a_successful_connect() {
    let mut s = Session::new(room());
    assert!(!s.is_joined());
    send(&mut s, &connect_packet("Nobody", &json!(7))).await;
    assert!(!s.is_joined(), "a refused Connect does not join");
    send(&mut s, &connect_packet("A", &json!(7))).await;
    assert!(s.is_joined());
}

#[tokio::test]
async fn a_join_print_carries_the_joiners_team_slot_and_tags() {
    let room = room();
    let mut watcher = joined(&room, "A").await;
    pump(&mut watcher).await;
    let mut b = Session::new(Arc::clone(&room));
    send(&mut b, &connect_packet("B", &json!(7))).await;
    assert_eq!(
        pump(&mut watcher).await,
        [json!({"cmd": "PrintJSON", "type": "Join",
            "data": [{"type": "player_id", "text": "2"}, {"text": " joined the room."}],
            "team": 0, "slot": 2, "tags": ["AP"]})]
    );
}

#[tokio::test]
async fn leaving_tells_the_others_with_a_part_print() {
    let room = room();
    let mut watcher = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut watcher).await;
    b.leave().await;
    assert!(!b.is_joined());
    assert_eq!(
        pump(&mut watcher).await,
        [json!({"cmd": "PrintJSON", "type": "Part",
            "data": [{"type": "player_id", "text": "2"}, {"text": " left the room."}],
            "team": 0, "slot": 2})]
    );
    b.leave().await;
    assert_eq!(pump(&mut watcher).await, Vec::<Value>::new(), "a second leave says nothing");
}

#[tokio::test]
async fn a_status_other_than_goal_announces_no_goal() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut b).await;
    // CLIENT_READY (10) and CLIENT_PLAYING (20) are not CLIENT_GOAL (30).
    for status in [10, 20] {
        send(&mut a, &json!([{"cmd": "StatusUpdate", "status": status}])).await;
    }
    assert_eq!(pump(&mut b).await, Vec::<Value>::new());
}

#[tokio::test]
async fn a_goal_print_names_the_player_and_skips_no_text_clients() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    let mut quiet = Session::new(Arc::clone(&room));
    let mut p = connect_packet("B", &json!(7));
    p[0]["tags"] = json!(["AP", "NoText"]);
    send(&mut quiet, &p).await;
    pump(&mut b).await;
    pump(&mut quiet).await;
    send(&mut a, &json!([{"cmd": "StatusUpdate", "status": 30}])).await;
    assert_eq!(
        pump(&mut b).await,
        [json!({"cmd": "PrintJSON", "type": "Goal",
            "data": [{"type": "player_id", "text": "1"}, {"text": " reached their goal."}],
            "team": 0, "slot": 1})]
    );
    assert_eq!(pump(&mut quiet).await, Vec::<Value>::new());
}

#[tokio::test]
async fn chat_and_command_prints_have_the_shape_clients_display() {
    let room = room();
    let mut a = joined(&room, "A").await;
    send(&mut a, &json!([{"cmd": "Say", "text": "hello"}])).await;
    // The reference server prefixes the sender's alias (MultiServer.py, ClientMessageProcessor).
    assert_eq!(
        pump(&mut a).await,
        [json!({"cmd": "PrintJSON", "type": "Chat", "data": [{"text": "A: hello"}],
            "team": 0, "slot": 1, "message": "hello"})]
    );
    assert_eq!(
        send(&mut a, &json!([{"cmd": "Say", "text": "!hint x"}])).await,
        [json!({"cmd": "PrintJSON", "type": "CommandResult",
            "data": [{"text": "Commands are not supported by this server yet."}]})]
    );
}

#[tokio::test]
async fn a_bounce_exactly_at_the_size_cap_is_accepted() {
    let mut a = joined(&limited(TIGHT), "A").await;
    let mut p = json!({"cmd": "Bounce", "data": ""});
    let pad = TIGHT.bounce_max_bytes - p.to_string().len();
    p["data"] = json!("y".repeat(pad));
    assert_eq!(p.to_string().len(), TIGHT.bounce_max_bytes);
    assert_eq!(send(&mut a, &json!([p])).await, Vec::<Value>::new());
}

#[tokio::test]
async fn a_client_without_a_game_or_a_no_game_tag_is_refused_invalid_game() {
    // The reference waives the game check only for an empty game with a no-game tag.
    let mut s = Session::new(room());
    let mut p = connect_packet("A", &json!(7));
    p[0]["game"] = json!("");
    assert_eq!(send(&mut s, &p).await[0]["errors"], json!(["InvalidGame"]));
}

#[tokio::test]
async fn a_client_at_exactly_the_minimum_version_connects() {
    // Slot A's minimum is 0.5.0; the reference refuses only a version below it.
    let mut s = Session::new(room());
    let mut p = connect_packet("A", &json!(7));
    p[0]["version"] = json!({"major": 0, "minor": 5, "build": 0});
    assert_eq!(send(&mut s, &p).await[0]["cmd"], "Connected");
}

#[tokio::test]
async fn another_slots_check_sends_no_room_update() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut a).await;
    check(&mut b, &[100]).await;
    // RoomUpdate's checked_locations are the receiving client's own new checks.
    assert_eq!(of(&pump(&mut a).await, "RoomUpdate"), Vec::<&Value>::new());
}

#[tokio::test]
async fn a_bounce_to_games_reaches_only_clients_of_those_games() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    pump(&mut b).await;
    send(&mut a, &json!([{"cmd": "Bounce", "games": ["Other"]}])).await;
    assert_eq!(pump(&mut b).await, Vec::<Value>::new(), "B plays G, not Other");
    send(&mut a, &json!([{"cmd": "Bounce", "games": [GAME]}])).await;
    assert_eq!(pump(&mut b).await, [json!({"cmd": "Bounced", "games": [GAME]})]);
}

#[tokio::test]
async fn slot_data_is_sent_when_connect_leaves_the_flag_out() {
    // The reference server defaults the flag to true (MultiServer.py, `args.get("slot_data", True)`).
    let mut s = Session::new(room());
    let mut p = connect_packet("A", &json!(7));
    p[0].as_object_mut().unwrap().remove("slot_data");
    assert_eq!(send(&mut s, &p).await[0]["slot_data"], json!({"answer": 42}));
}

/// Slot A starts with items 8 and 7 (protocol doc, "`items_handling` flags" bit 2 and "Data
/// Package Contents": location -2, player 0).
fn start_room() -> Arc<CompatRoom> {
    room_edit(|v| v["slots"][0]["start_inventory"] = json!([8, 7]))
}

fn start_item(item: i64) -> Value {
    json!({"item": item, "location": -2, "player": 0, "flags": 0})
}

#[tokio::test]
async fn a_client_with_the_start_inventory_flag_gets_start_items_first_from_index_zero() {
    let room = start_room();
    let mut a = Session::new(Arc::clone(&room));
    let ps = send(&mut a, &connect_packet("A", &json!(7))).await;
    assert_eq!(
        ps[1],
        json!({"cmd": "ReceivedItems", "index": 0, "items": [start_item(8), start_item(7)]})
    );
    pump(&mut a).await;
    let mut b = joined(&room, "B").await;
    check(&mut b, &[100]).await;
    // Live items continue after the start inventory in the index.
    let got = pump(&mut a).await;
    assert_eq!(
        *of(&got, "ReceivedItems")[0],
        json!({"cmd": "ReceivedItems", "index": 2,
               "items": [{"item": 7, "location": 100, "player": 2, "flags": 4}]})
    );
    let sync = send(&mut a, &json!([{"cmd": "Sync"}])).await;
    assert_eq!(sync[0]["items"].as_array().unwrap().len(), 3, "{sync:?}");
    // No print for a start item: the only prints are the check's.
    assert!(got.iter().all(|p| p["item"]["location"] != -2), "{got:?}");
}

#[tokio::test]
async fn a_client_without_the_flag_never_sees_start_items_and_indexes_from_zero() {
    let room = start_room();
    let mut a = Session::new(Arc::clone(&room));
    let ps = send(&mut a, &connect_packet("A", &json!(3))).await;
    assert_eq!(of(&ps, "ReceivedItems"), Vec::<&Value>::new());
    pump(&mut a).await;
    let mut b = joined(&room, "B").await;
    check(&mut b, &[100]).await;
    assert_eq!(of(&pump(&mut a).await, "ReceivedItems")[0]["index"], 0);
    // Another slot's start inventory is never sent to B.
    assert_eq!(of(&pump(&mut b).await, "ReceivedItems"), Vec::<&Value>::new());
}

#[tokio::test]
async fn turning_on_the_start_inventory_flag_resends_everything_from_zero() {
    let room = start_room();
    let mut a = Session::new(Arc::clone(&room));
    send(&mut a, &connect_packet("A", &json!(3))).await;
    check(&mut a, &[101]).await;
    pump(&mut a).await;
    let ps = send(&mut a, &json!([{"cmd": "ConnectUpdate", "items_handling": 7}])).await;
    assert_eq!(
        ps[0],
        json!({"cmd": "ReceivedItems", "index": 0, "items": [start_item(8), start_item(7),
               {"item": 8, "location": 101, "player": 1, "flags": 0}]})
    );
}

#[tokio::test]
async fn the_archipelago_game_is_listed_and_its_package_is_served_without_name_groups() {
    let room = room_edit(|v| {
        v["games"]["Archipelago"] = json!({"item_name_to_id": {"Nothing": -1},
            "location_name_to_id": {"Server": -2}, "checksum": "ap"});
        v["games"][GAME]["item_name_groups"] = json!({"Both": ["Seven", "Eight"]});
    });
    let mut s = Session::new(room);
    let info = &packets(&[s.hello()])[0];
    assert_eq!(info["games"], json!(["Archipelago", GAME]));
    assert_eq!(info["datapackage_checksums"]["Archipelago"], "ap");
    let ps = send(&mut s, &json!([{"cmd": "GetDataPackage"}])).await;
    let games = &ps[0]["data"]["games"];
    assert_eq!(games["Archipelago"]["location_name_to_id"]["Server"], -2);
    // Clients read groups through `Get`, never in the package (protocol doc, "Get").
    let keys: Vec<&String> = games[GAME].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["checksum", "item_name_to_id", "location_name_to_id"]);
}
