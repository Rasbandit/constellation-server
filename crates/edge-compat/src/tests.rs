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
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, config::Limits::DESIGN);
    Arc::new(CompatRoom::new(b, h, override_pw.map(str::to_owned)).unwrap())
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
    loop {
        let next = match s.live() {
            Some(rx) => rx.try_recv(),
            None => return Vec::new(),
        };
        match next {
            Ok(p) => frames.extend(s.on_published(&p).unwrap()),
            Err(TryRecvError::Empty) => return packets(&frames),
            Err(TryRecvError::Disconnected) => frames.extend(s.resync().await.unwrap()),
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
        (connect_packet("A", &json!(8)), "InvalidItemsHandling"),
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
async fn scouts_answer_with_the_receiver_and_skip_unknown_locations() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let ps = send(&mut a, &json!([{"cmd": "LocationScouts", "locations": [101, 100, 5]}])).await;
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
fn big_room(end: i64, limits: config::Limits) -> Arc<CompatRoom> {
    let mut v: Value = serde_json::to_value(&*bundle(&Value::Null)).unwrap();
    let locs: Vec<Value> =
        (1000..end).map(|l| json!({"location": l, "item": 7, "receiver": 2, "flags": 0})).collect();
    v["slots"][0]["locations"] = json!(locs);
    for l in 1000..end {
        v["games"][GAME]["location_name_to_id"][format!("L{l}")] = json!(l);
    }
    let b = Arc::new(SeedBundle::from_json(&v.to_string()).unwrap());
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let state = RoomState::new(b.slot_nos(), b.placement()).unwrap();
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, limits);
    Arc::new(CompatRoom::new(b, h, None).unwrap())
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
    let room = big_room(1200, config::Limits::DESIGN);
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
    let room = big_room(1030, config::Limits { session_queue: 1, ..config::Limits::DESIGN });
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
