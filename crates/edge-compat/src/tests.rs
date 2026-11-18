// Session tests over a real room task and MemLogStore. Expected packets come from the protocol
// document's field tables and from the hand-written bundle below (who receives what is read
// off it by eye).
use super::*;
use log::MemLogStore;
use proto::RoomId;
use serde_json::{Value, json};
use tokio::sync::mpsc::error::TryRecvError;

const GAME: &str = "G";

/// Slot 1 (A): location 100 -> B's item 7 (progression), 101 -> A's own item 8.
/// Slot 2 (B): location 100 -> A's item 7 (trap).
/// A v1 bundle as the room runs it (encoded to v2 in memory, the server's load path).
fn seed(v: &Value) -> Arc<Seed> {
    Arc::new(seed_bundle::import_v1(v.to_string().as_bytes(), 1 << 26).unwrap().0)
}

fn state_of(b: &Arc<Seed>) -> room_core::RoomState {
    seed_bundle::room_state(b).unwrap()
}

fn bundle(password: &Value) -> Arc<Seed> {
    seed(&bundle_json(password))
}

/// The bundle above as v1 JSON.
fn bundle_json(password: &Value) -> Value {
    json!({
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
    })
}

fn room_with(password: &Value, override_pw: Option<&str>) -> Arc<CompatRoom> {
    let b = bundle(password);
    spawn_with(b, override_pw, &Limits::DESIGN)
}

/// A room task for `b` on an in-memory log, and its compatible edge.
fn spawn_with(b: Arc<Seed>, password: Option<&str>, limits: &Limits) -> Arc<CompatRoom> {
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let state = state_of(&b);
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state, store, *limits);
    Arc::new(CompatRoom::new(b, h, password.map(str::to_owned), *limits).unwrap())
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
            Ok(p) => frames.extend(s.on_published(&p).await.unwrap()),
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
        json!({"class": "NetworkSlot", "name": "B", "game": GAME, "type": 1, "group_members": []})
    );
    assert_eq!(
        c["players"][1],
        json!({"class": "NetworkPlayer", "team": 0, "slot": 2, "alias": "B", "name": "B"})
    );
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
        "items": [{"class": "NetworkItem", "item": 7, "location": 100, "player": 1, "flags": 1}]})
    );
    let to_a = pump(&mut a).await;
    assert_eq!(of(&to_a, "ReceivedItems"), Vec::<&Value>::new());
    assert_eq!(
        *of(&to_a, "RoomUpdate")[0],
        json!({"cmd": "RoomUpdate", "checked_locations": [100], "hint_points": 2})
    );
    let send = of(&to_a, "PrintJSON").into_iter().find(|p| p["type"] == "ItemSend").unwrap();
    assert_eq!(send["receiving"], 2);
    assert_eq!(
        send["item"],
        json!({"class": "NetworkItem", "item": 7, "location": 100, "player": 1, "flags": 1})
    );
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
        json!([{"class": "NetworkItem", "item": 8, "location": 101, "player": 1, "flags": 0}])
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
        {"class": "NetworkItem", "item": 8, "location": 101, "player": 1, "flags": 0},
        {"class": "NetworkItem", "item": 7, "location": 100, "player": 2, "flags": 4}]})
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
        "_read_client_status_0_1": 30, "_read_client_status_0_2": 5, "free": null,
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
    let ps = send(&mut a, &json!([{"cmd": "Say", "text": "!release"}])).await;
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
        {"class": "NetworkItem", "item": 7, "location": 100, "player": 2, "flags": 1},
        {"class": "NetworkItem", "item": 8, "location": 101, "player": 1, "flags": 0}]})]
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
    for cmd in ["CreateHints"] {
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
fn big_room(end: i64, limits: &Limits) -> Arc<CompatRoom> {
    big_room_to(end, 2, limits)
}

/// As [`big_room`], every item going to `receiver`.
fn big_room_to(end: i64, receiver: u32, limits: &Limits) -> Arc<CompatRoom> {
    let mut v: Value = bundle_json(&Value::Null);
    let locs: Vec<Value> = (1000..end)
        .map(|l| json!({"location": l, "item": 7, "receiver": receiver, "flags": 0}))
        .collect();
    v["slots"][0]["locations"] = json!(locs);
    for l in 1000..end {
        v["games"][GAME]["location_name_to_id"][format!("L{l}")] = json!(l);
    }
    spawn_with(seed(&v), None, limits)
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
    let room = big_room(1200, &Limits::DESIGN);
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
    let room = big_room(1030, &Limits { session_queue: 1, ..Limits::DESIGN });
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

fn limited(limits: &Limits) -> Arc<CompatRoom> {
    let b = bundle(&Value::Null);
    spawn_with(b, None, limits)
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
    let room = limited(&TIGHT);
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
    let room = limited(&TIGHT);
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
    let mut s = Session::new(limited(&TIGHT));
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

/// A byte bucket may go into debt by one charge, and nothing is ready until the debt is
/// repaid by refill: the long-run rate is the refill rate, whatever each charge costs.
#[test]
fn a_charged_bucket_runs_into_debt_and_refills_it_before_the_next_charge() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let mut b = limit::Bucket::new(100, 10_000, t0); // 100 bytes, 10 per second
    assert!(b.ready(t0));
    b.charge(60, t0);
    assert!(b.ready(t0), "40 bytes left");
    b.charge(70, t0); // 30 in debt
    assert!(!b.ready(t0));
    assert!(!b.ready(t0 + Duration::from_secs(3)), "the debt is repaid, no byte to spend yet");
    assert!(b.ready(t0 + Duration::from_millis(3100)));
    let mut b = limit::Bucket::new(100, 10_000, t0);
    b.charge(u64::MAX, t0);
    assert!(!b.ready(t0 + Duration::from_hours(24)), "a huge debt stays a debt");
}

/// The test bundle with an edit applied.
fn room_edit(edit: impl FnOnce(&mut Value)) -> Arc<CompatRoom> {
    let mut v: Value = bundle_json(&Value::Null);
    edit(&mut v);
    spawn_with(seed(&v), None, &Limits::DESIGN)
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
    // Connected (5), the status of any slot with a connection, and not the goal.
    assert_eq!(ps[0]["keys"]["_read_client_status_0_1"], 5);
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

/// Two games with disjoint item ids (`crates/e2e/fixtures/cross-games.json`): P1 plays A and
/// finds B's item 202 for P2; P2 plays B and finds A's item 2 (a trap) for P1; P3 places
/// nothing. Ids at the wire come from the receiver's game.
#[tokio::test]
async fn items_across_games_carry_the_receivers_ids_at_the_wire() {
    let v: Value =
        serde_json::from_str(include_str!("../../e2e/fixtures/cross-games.json")).unwrap();
    let b = seed(&v);
    let store = Arc::new(MemLogStore::new());
    let epoch = store.claim(RoomId(1));
    let (h, _task) = RoomHandle::spawn(RoomId(1), epoch, state_of(&b), store, Limits::DESIGN);
    let room = Arc::new(CompatRoom::new(b, h, None, Limits::DESIGN).unwrap());
    let join = async |name: &str, game: &str| {
        let mut s = Session::new(Arc::clone(&room));
        let mut c = connect_packet(name, &json!(7));
        c[0]["game"] = json!(game);
        let ps = send(&mut s, &c).await;
        assert_eq!(ps[0]["cmd"], "Connected", "{ps:?}");
        pump(&mut s).await;
        s
    };
    let (mut p1, mut p2, mut p3) =
        (join("P1", "A").await, join("P2", "B").await, join("P3", "A").await);
    let ps = send(&mut p1, &json!([{"cmd": "LocationScouts", "locations": [11]}])).await;
    assert_eq!(
        ps[0]["locations"],
        json!([{"class": "NetworkItem", "item": 202, "location": 11, "player": 2, "flags": 1}])
    );
    check(&mut p1, &[11]).await;
    check(&mut p2, &[21]).await;
    let to_p2 = pump(&mut p2).await;
    assert_eq!(
        of(&to_p2, "ReceivedItems")[0]["items"],
        json!([{"class": "NetworkItem", "item": 202, "location": 11, "player": 1, "flags": 1}])
    );
    let to_p1 = pump(&mut p1).await;
    assert_eq!(
        of(&to_p1, "ReceivedItems")[0]["items"],
        json!([{"class": "NetworkItem", "item": 2, "location": 21, "player": 2, "flags": 4}])
    );
    // A reconnect rebuilds the same inventory from the log and the seed.
    let again = send(&mut Session::new(Arc::clone(&room)), &{
        let mut c = connect_packet("P2", &json!(7));
        c[0]["game"] = json!("B");
        c
    })
    .await;
    assert_eq!(of(&again, "ReceivedItems")[0]["items"][0]["item"], 202);
    let ps = send(&mut p3, &json!([{"cmd": "LocationScouts", "locations": [11]}])).await;
    assert_eq!(ps[0]["cmd"], "InvalidPacket", "P3 places nothing: {ps:?}");
    assert_eq!(pump(&mut p3).await.iter().filter(|p| p["cmd"] == "ReceivedItems").count(), 0);
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
    let room = limited(&TIGHT);
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
    let room = limited(&TIGHT);
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
        limited(&Limits { connect_burst: 2, connect_refill_milli_per_sec: 0, ..Limits::DESIGN });
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
    let room = limited(&Limits { connect_burst: 1000, ..Limits::DESIGN });
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
    let room = big_room_to(1030, 1, &Limits { session_queue: 1, ..Limits::DESIGN });
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
        send(&mut a, &json!([{"cmd": "Say", "text": "!release"}])).await,
        [json!({"cmd": "PrintJSON", "type": "CommandResult",
            "data": [{"text": "!release is not supported by this server yet."}]})]
    );
}

#[tokio::test]
async fn a_bounce_exactly_at_the_size_cap_is_accepted() {
    let mut a = joined(&limited(&TIGHT), "A").await;
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
    json!({"class": "NetworkItem", "item": item, "location": -2, "player": 0, "flags": 0})
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
               "items": [{"class": "NetworkItem", "item": 7, "location": 100, "player": 2, "flags": 4}]})
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
               {"class": "NetworkItem", "item": 8, "location": 101, "player": 1, "flags": 0}]})
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

// Data storage (protocol doc, sections "Get", "Set", "SetNotify", "SetReply"; reference
// `MultiServer.py:2271-2312`). Expected values are hand calculations of the operations.

fn set(key: &str, ops: &Value) -> Value {
    json!([{"cmd": "Set", "key": key, "operations": ops}])
}

fn set_reply_of(ps: &[Value]) -> Vec<&Value> {
    of(ps, "SetReply")
}

async fn watch(s: &mut Session, keys: &[&str]) {
    assert_eq!(send(s, &json!([{"cmd": "SetNotify", "keys": keys}])).await, Vec::<Value>::new());
}

async fn get(s: &mut Session, keys: &[&str]) -> Value {
    let ps = send(s, &json!([{"cmd": "Get", "keys": keys}])).await;
    assert_eq!(ps[0]["cmd"], "Retrieved", "{ps:?}");
    ps[0]["keys"].clone()
}

#[tokio::test]
async fn a_set_with_add_replies_and_notifies_subscribers() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    watch(&mut a, &["k"]).await;
    let p = json!([{"cmd": "Set", "key": "k", "default": 10, "want_reply": true,
                    "operations": [{"operation": "add", "value": 5}]}]);
    let want = json!({"cmd": "SetReply", "key": "k", "default": 10, "want_reply": true,
                      "operations": [{"operation": "add", "value": 5}],
                      "value": 15, "original_value": 10, "slot": 2});
    assert_eq!(send(&mut b, &p).await, std::slice::from_ref(&want));
    assert_eq!(set_reply_of(&pump(&mut a).await), [&want]);
    assert_eq!(set_reply_of(&pump(&mut b).await), Vec::<&Value>::new(), "b already has its reply");
    assert_eq!(get(&mut a, &["k"]).await, json!({"k": 15}));
}

#[tokio::test]
async fn a_set_without_want_reply_and_no_subscribers_sends_nothing() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    watch(&mut a, &["other"]).await;
    for want_reply in [json!(false), json!(0), json!(""), Value::Null] {
        let mut p = set("k", &json!([{"operation": "replace", "value": 1}]));
        p[0]["want_reply"] = want_reply;
        assert_eq!(send(&mut b, &p).await, Vec::<Value>::new());
    }
    assert_eq!(set_reply_of(&pump(&mut a).await), Vec::<&Value>::new());
    assert_eq!(set_reply_of(&pump(&mut b).await), Vec::<&Value>::new());
    // Any truthy value asks for a reply, as Python reads it.
    let mut p = set("k", &json!([{"operation": "replace", "value": 2}]));
    p[0]["want_reply"] = json!(1);
    assert_eq!(set_reply_of(&send(&mut b, &p).await).len(), 1);
}

#[tokio::test]
async fn a_setter_that_also_subscribes_gets_one_set_reply() {
    let room = room();
    let mut a = joined(&room, "A").await;
    watch(&mut a, &["k"]).await;
    for want_reply in [true, false] {
        let mut p = set("k", &json!([{"operation": "add", "value": 1}]));
        p[0]["want_reply"] = json!(want_reply);
        let mut got = send(&mut a, &p).await;
        got.extend(pump(&mut a).await);
        assert_eq!(set_reply_of(&got).len(), 1, "want_reply {want_reply}: {got:?}");
    }
}

#[tokio::test]
async fn set_reply_echoes_every_extra_field_of_the_set() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    watch(&mut a, &["k"]).await;
    // Fields named like the reply's own are overwritten by the server's values.
    let p = json!([{"cmd": "Set", "key": "k", "want_reply": true, "tag": {"n": [1]},
                    "slot": 99, "value": "client", "original_value": "client",
                    "operations": [{"operation": "replace", "value": "x"}]}]);
    let mine = send(&mut b, &p).await;
    let theirs = pump(&mut a).await;
    for r in [&mine[0], set_reply_of(&theirs)[0]] {
        assert_eq!(r["tag"], json!({"n": [1]}), "{r}");
        assert_eq!((r["slot"].clone(), r["value"].clone()), (json!(2), json!("x")));
        assert_eq!(r["original_value"], 0);
        assert_eq!(r["operations"], json!([{"operation": "replace", "value": "x"}]));
    }
}

#[tokio::test]
async fn a_set_with_no_operations_still_stores_the_default() {
    let room = room();
    let mut a = joined(&room, "A").await;
    send(&mut a, &json!([{"cmd": "Set", "key": "d", "default": {"x": 1}, "operations": []}])).await;
    send(&mut a, &json!([{"cmd": "Set", "key": "z", "operations": [{"operation": "default"}]}]))
        .await;
    assert_eq!(get(&mut a, &["d", "z"]).await, json!({"d": {"x": 1}, "z": 0}));
}

#[tokio::test]
async fn a_refused_operation_changes_nothing_and_the_connection_stays_open() {
    let room = room();
    let mut a = joined(&room, "A").await;
    send(&mut a, &set("k", &json!([{"operation": "replace", "value": [1]}]))).await;
    let bad =
        set("k", &json!([{"operation": "add", "value": [2]}, {"operation": "mod", "value": 0}]));
    let ps = send(&mut a, &bad).await;
    assert_eq!(
        (ps[0]["cmd"].clone(), ps[0]["type"].clone()),
        (json!("InvalidPacket"), json!("arguments"))
    );
    assert!(ps[0]["text"].as_str().unwrap().contains("nothing was stored"), "{ps:?}");
    assert_eq!(get(&mut a, &["k"]).await, json!({"k": [1]}));
    for bad in [
        json!([{"cmd": "Set", "operations": []}]),
        json!([{"cmd": "Set", "key": 1, "operations": []}]),
        json!([{"cmd": "Set", "key": "k"}]),
        json!([{"cmd": "Set", "key": "k", "operations": {}}]),
        set("k", &json!([{"operation": "frobnicate", "value": 1}])),
        set("k", &json!([{"value": 1}])),
        set("k", &json!([{"operation": "add"}])),
        json!([{"cmd": "Set", "key": "k", "operations": [], "default": [[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]}]),
    ] {
        let ps = send(&mut a, &bad).await;
        assert_eq!((ps.len(), ps[0]["type"].clone()), (1, json!("arguments")), "{bad}: {ps:?}");
    }
    assert_eq!(get(&mut a, &["k"]).await, json!({"k": [1]}));
}

#[tokio::test]
async fn a_reserved_key_cannot_be_set_but_a_key_like_readme_can() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let ps =
        send(&mut a, &set("_read_race_mode", &json!([{"operation": "replace", "value": 1}]))).await;
    assert!(ps[0]["text"].as_str().unwrap().contains("read only"), "{ps:?}");
    assert_eq!(get(&mut a, &["_read_race_mode"]).await, json!({"_read_race_mode": 0}));
    send(&mut a, &set("_readme", &json!([{"operation": "replace", "value": 1}]))).await;
    assert_eq!(get(&mut a, &["_readme"]).await, json!({"_readme": 1}));
}

#[tokio::test]
async fn a_value_over_the_cap_and_a_runaway_multiply_are_refused() {
    let room = limited(&Limits { kv_max_value_bytes: 16, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    let big = set("k", &json!([{"operation": "replace", "value": "x".repeat(15)}]));
    assert!(send(&mut a, &big).await[0]["text"].as_str().unwrap().contains("too large"));
    let runaway = set(
        "k",
        &json!([{"operation": "replace", "value": [1]},
                                   {"operation": "mul", "value": 1_000_000_000_000_i64}]),
    );
    assert!(send(&mut a, &runaway).await[0]["text"].as_str().unwrap().contains("too large"));
    assert_eq!(get(&mut a, &["k"]).await, json!({"k": null}));
}

#[tokio::test]
async fn get_returns_null_for_unknown_keys_and_echoes_extra_fields() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let ps = send(
        &mut a,
        &json!([{"cmd": "Get", "keys": ["nope", "_read_nope", "_read_hints_0_01",
        "_read_hints_1_1", "_read_hints_0_9", "_read_slot_data_01", "_read_client_status_0_9"],
        "tag": [1], "id": "x"}]),
    )
    .await;
    assert_eq!(
        ps,
        [json!({"cmd": "Retrieved", "tag": [1], "id": "x", "keys": {
        "nope": null, "_read_nope": null, "_read_hints_0_01": null, "_read_hints_1_1": null,
        "_read_hints_0_9": null, "_read_slot_data_01": null, "_read_client_status_0_9": null}})]
    );
    assert_eq!(
        send(&mut a, &json!([{"cmd": "Get", "keys": []}])).await,
        [json!({"cmd": "Retrieved", "keys": {}})]
    );
    for bad in [
        json!([{"cmd": "Get"}]),
        json!([{"cmd": "Get", "keys": "k"}]),
        json!([{"cmd": "Get", "keys": [1]}]),
    ] {
        assert_eq!(send(&mut a, &bad).await[0]["type"], "arguments", "{bad}");
    }
}

#[tokio::test]
async fn get_set_and_set_notify_need_a_connection_first() {
    let mut s = Session::new(room());
    for p in [
        json!([{"cmd": "Get", "keys": []}]),
        set("k", &json!([])),
        json!([{"cmd": "SetNotify", "keys": ["k"]}]),
    ] {
        let ps = send(&mut s, &p).await;
        assert_eq!(
            (ps[0]["cmd"].clone(), ps[0]["type"].clone()),
            (json!("InvalidPacket"), json!("cmd"))
        );
    }
}

#[tokio::test]
async fn name_groups_are_readable_for_every_game_in_the_room() {
    let room = room_edit(|v| {
        v["games"][GAME]["item_name_groups"] = json!({"Both": ["Seven", "Eight"]});
        v["games"][GAME]["location_name_groups"] = json!({"All": ["L100", "L101"]});
        v["games"]["Archipelago"] = json!({"item_name_to_id": {}, "location_name_to_id": {},
            "checksum": "ap", "item_name_groups": {"Everything": []}});
    });
    let mut a = joined(&room, "A").await;
    let keys = [
        "_read_item_name_groups_G",
        "_read_location_name_groups_G",
        "_read_item_name_groups_Archipelago",
        "_read_location_name_groups_Archipelago",
        "_read_item_name_groups_Other",
    ];
    assert_eq!(
        get(&mut a, &keys).await,
        json!({
        "_read_item_name_groups_G": {"Both": ["Seven", "Eight"]},
        "_read_location_name_groups_G": {"All": ["L100", "L101"]},
        "_read_item_name_groups_Archipelago": {"Everything": []},
        "_read_location_name_groups_Archipelago": {},
        "_read_item_name_groups_Other": null})
    );
}

#[tokio::test]
async fn subscriptions_end_with_the_connection_and_a_reconnect_must_subscribe_again() {
    let room = room();
    let mut a = joined(&room, "A").await;
    watch(&mut a, &["k"]).await;
    a.leave().await;
    let mut again = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    send(&mut b, &set("k", &json!([{"operation": "add", "value": 1}]))).await;
    assert_eq!(set_reply_of(&pump(&mut again).await), Vec::<&Value>::new());
    watch(&mut again, &["k"]).await;
    send(&mut b, &set("k", &json!([{"operation": "add", "value": 1}]))).await;
    assert_eq!(set_reply_of(&pump(&mut again).await)[0]["value"], 2);
    // A re-Connect on the same socket keeps its watches: it is the same connection.
    send(&mut again, &connect_packet("A", &json!(7))).await;
    pump(&mut again).await;
    send(&mut b, &set("k", &json!([{"operation": "add", "value": 1}]))).await;
    assert_eq!(set_reply_of(&pump(&mut again).await)[0]["value"], 3);
}

#[tokio::test]
async fn a_subscriber_that_fell_behind_still_converges_on_the_latest_value() {
    // A one-entry queue: the watcher falls behind and resyncs from the log, without notes.
    let room = limited(&Limits { session_queue: 1, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    let mut b = joined(&room, "B").await;
    watch(&mut a, &["k"]).await;
    pump(&mut a).await; // B's join print, so the first write fits the queue
    for _ in 0..5 {
        let p = json!([{"cmd": "Set", "key": "k", "tag": 1, "operations": [{"operation": "add", "value": 2}]}]);
        send(&mut b, &p).await;
    }
    let replies: Vec<Value> = set_reply_of(&pump(&mut a).await).into_iter().cloned().collect();
    let values: Vec<i64> = replies.iter().map(|r| r["value"].as_i64().unwrap()).collect();
    assert_eq!(values, [2, 4, 6, 8, 10], "every write once, in order");
    // The first came with its note; the ones read back from the log have no pass-through
    // fields and an unknown original value.
    assert_eq!(
        (replies[0]["tag"].clone(), replies[0]["original_value"].clone()),
        (json!(1), json!(0))
    );
    let resynced = replies.last().unwrap();
    assert_eq!(
        *resynced,
        json!({"cmd": "SetReply", "key": "k", "value": 10, "original_value": null, "slot": 2})
    );
}

#[tokio::test]
async fn set_notify_is_bounded_per_packet_and_per_connection() {
    let room =
        limited(&Limits { kv_max_keys_per_request: 2, kv_max_watched_keys: 3, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    let too_many = send(&mut a, &json!([{"cmd": "SetNotify", "keys": ["a", "b", "c"]}])).await;
    assert!(too_many[0]["text"].as_str().unwrap().contains("too many keys"), "{too_many:?}");
    let too_many = send(&mut a, &json!([{"cmd": "Get", "keys": ["a", "b", "c"]}])).await;
    assert!(too_many[0]["text"].as_str().unwrap().contains("too many keys"), "{too_many:?}");
    watch(&mut a, &["a", "b"]).await;
    watch(&mut a, &["a", "c"]).await; // "a" again costs nothing
    let full = send(&mut a, &json!([{"cmd": "SetNotify", "keys": ["d"]}])).await;
    assert!(full[0]["text"].as_str().unwrap().contains("watched"), "{full:?}");
    for bad in [json!([{"cmd": "SetNotify"}]), json!([{"cmd": "SetNotify", "keys": "a"}])] {
        assert_eq!(send(&mut a, &bad).await[0]["type"], "arguments");
    }
}

#[tokio::test]
async fn set_is_rate_limited_per_slot_without_muting_chat() {
    let room =
        limited(&Limits { kv_set_burst: 2, kv_set_refill_milli_per_sec: 0, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    let p = set("k", &json!([{"operation": "add", "value": 1}]));
    assert_eq!(send(&mut a, &p).await, Vec::<Value>::new());
    assert_eq!(send(&mut a, &p).await, Vec::<Value>::new());
    assert!(send(&mut a, &p).await[0]["text"].as_str().unwrap().contains("rate limited"));
    assert_eq!(send(&mut a, &json!([{"cmd": "Say", "text": "hi"}])).await, Vec::<Value>::new());
    // A malformed Set is refused before it costs a token; the other slot has its own bucket.
    let mut b = joined(&room, "B").await;
    assert_eq!(send(&mut b, &p).await, Vec::<Value>::new());
    assert_eq!(get(&mut a, &["k"]).await, json!({"k": 3}));
}

#[tokio::test]
async fn client_status_changes_reach_subscribers_of_the_status_key() {
    let room = room();
    let mut a = joined(&room, "A").await;
    watch(&mut a, &["_read_client_status_0_2"]).await;
    assert_eq!(
        get(&mut a, &["_read_client_status_0_2", "_read_client_status_0_1"]).await,
        json!({"_read_client_status_0_2": 0, "_read_client_status_0_1": 5})
    );
    let status = |ps: &[Value]| -> Vec<Value> {
        set_reply_of(ps).iter().map(|r| r["value"].clone()).collect()
    };
    let reply = |v: i64| json!({"cmd": "SetReply", "key": "_read_client_status_0_2", "value": v});
    let mut b = joined(&room, "B").await;
    assert_eq!(set_reply_of(&pump(&mut a).await), [&reply(5)]);
    // A second connection to the slot changes nothing; leaving it changes nothing either.
    let mut b2 = joined(&room, "B").await;
    b2.leave().await;
    assert_eq!(status(&pump(&mut a).await), Vec::<Value>::new());
    send(&mut b, &json!([{"cmd": "StatusUpdate", "status": 20}])).await;
    send(&mut b, &json!([{"cmd": "StatusUpdate", "status": 20}])).await;
    assert_eq!(status(&pump(&mut a).await), [json!(20), json!(20)], "sent even when unchanged");
    assert_eq!(get(&mut a, &["_read_client_status_0_2"]).await["_read_client_status_0_2"], 20);
    b.leave().await;
    assert_eq!(status(&pump(&mut a).await), [json!(0)]);
    let mut b = joined(&room, "B").await;
    send(&mut b, &json!([{"cmd": "StatusUpdate", "status": 30}])).await;
    assert_eq!(status(&pump(&mut a).await), [json!(5), json!(30)]);
    // The goal is never undone: later updates and leaving say nothing.
    send(&mut b, &json!([{"cmd": "StatusUpdate", "status": 10}])).await;
    b.leave().await;
    assert_eq!(status(&pump(&mut a).await), Vec::<Value>::new());
    assert_eq!(get(&mut a, &["_read_client_status_0_2"]).await["_read_client_status_0_2"], 30);
}

#[tokio::test]
async fn a_spectator_slot_reads_as_done() {
    let room = room_edit(|v| v["slots"][1]["kind"] = json!("spectator"));
    let mut a = joined(&room, "A").await;
    assert_eq!(get(&mut a, &["_read_client_status_0_2"]).await["_read_client_status_0_2"], 30);
}

/// The reference server tags `NetworkItem`, `NetworkPlayer` and `NetworkSlot` with a `class`
/// field (`NetUtils.py:98-107`), and its client library builds them only from tagged objects
/// (`NetUtils.py:147-170`): without the tag, `CommonClient` reads no player names and crashes on
/// the first print naming a player (found by `just ap-client-check`).
#[tokio::test]
async fn network_items_players_and_slots_carry_their_class() {
    let room = room();
    let mut a = Session::new(Arc::clone(&room));
    let ps = send(&mut a, &connect_packet("A", &json!(7))).await;
    assert_eq!(ps[0]["players"][0]["class"], "NetworkPlayer", "{}", ps[0]);
    assert_eq!(ps[0]["slot_info"]["1"]["class"], "NetworkSlot");
    check(&mut a, &[101]).await;
    let got = pump(&mut a).await;
    assert_eq!(of(&got, "ReceivedItems")[0]["items"][0]["class"], "NetworkItem");
    let send_print = of(&got, "PrintJSON").into_iter().find(|p| p["type"] == "ItemSend").unwrap();
    assert_eq!(send_print["item"]["class"], "NetworkItem");
    let ps = send(&mut a, &json!([{"cmd": "LocationScouts", "locations": [100]}])).await;
    assert_eq!(ps[0]["locations"][0]["class"], "NetworkItem");
}

/// Every operation name of the protocol document ("`DataStorageOperation`") reaches its own
/// operation; results are hand calculations.
#[tokio::test]
async fn every_operation_name_maps_to_its_operation() {
    let room = room();
    let mut a = joined(&room, "A").await;
    let cases = [
        ("replace", json!(1), json!(9), json!(9)),
        ("default", json!(1), json!(9), json!(1)),
        ("add", json!(1), json!(2), json!(3)),
        ("mul", json!(3), json!(2), json!(6)),
        ("pow", json!(2), json!(3), json!(8)),
        ("mod", json!(7), json!(4), json!(3)),
        ("floor", json!(2.5), json!(0), json!(2)),
        ("ceil", json!(2.5), json!(0), json!(3)),
        ("max", json!(1), json!(5), json!(5)),
        ("min", json!(5), json!(1), json!(1)),
        ("and", json!(12), json!(10), json!(8)),
        ("or", json!(12), json!(10), json!(14)),
        ("xor", json!(12), json!(10), json!(6)),
        ("left_shift", json!(1), json!(2), json!(4)),
        ("right_shift", json!(8), json!(1), json!(4)),
        ("remove", json!([1, 2]), json!(1), json!([2])),
        ("pop", json!([1, 2]), json!(0), json!([2])),
        ("update", json!([1]), json!([2]), json!([1, 2])),
    ];
    for (name, default, value, want) in cases {
        let p = json!([{"cmd": "Set", "key": name, "default": default, "want_reply": true,
                        "operations": [{"operation": name, "value": value}]}]);
        let ps = send(&mut a, &p).await;
        assert_eq!(ps[0]["value"], want, "{name}: {ps:?}");
    }
}

/// `want_reply` is read with Python truthiness: empty strings, lists and objects are false.
#[tokio::test]
async fn want_reply_is_read_with_python_truthiness() {
    let room = room();
    let mut a = joined(&room, "A").await;
    for (flag, replies) in [
        (json!(""), 0),
        (json!("x"), 1),
        (json!([]), 0),
        (json!([0]), 1),
        (json!({}), 0),
        (json!({"a": 0}), 1),
        (json!(0.0), 0),
        (json!(0.5), 1),
    ] {
        let mut p = set("k", &json!([{"operation": "replace", "value": 1}]));
        p[0]["want_reply"] = flag.clone();
        assert_eq!(set_reply_of(&send(&mut a, &p).await).len(), replies, "{flag}");
    }
}

/// The goal (30) is terminal for watchers even when another connection of the same slot sends
/// a status at the same moment: its goal check and its publish must not straddle the goal
/// (adversarial review on PR 46). `join!` polls the status first, so under the old code it
/// read "no goal" before the goal landed and published 20 after the 30.
#[tokio::test]
async fn a_status_racing_the_goal_from_another_connection_never_follows_it() {
    let room = room();
    let mut a = joined(&room, "A").await;
    watch(&mut a, &["_read_client_status_0_2"]).await;
    let mut b1 = joined(&room, "B").await;
    let mut b2 = joined(&room, "B").await;
    pump(&mut a).await;
    let late = json!([{"cmd": "StatusUpdate", "status": 20}]);
    let goal = json!([{"cmd": "StatusUpdate", "status": 30}]);
    tokio::join!(send(&mut b2, &late), send(&mut b1, &goal));
    let values: Vec<Value> =
        set_reply_of(&pump(&mut a).await).iter().map(|r| r["value"].clone()).collect();
    assert_eq!(values.last(), Some(&json!(30)), "{values:?}");
    assert_eq!(get(&mut a, &["_read_client_status_0_2"]).await["_read_client_status_0_2"], 30);
    // And nothing after it, from any connection of the slot.
    send(&mut b2, &late).await;
    b1.leave().await;
    b2.leave().await;
    assert_eq!(set_reply_of(&pump(&mut a).await), Vec::<&Value>::new());
}

/// Spectators count as done from the start and never change (reference `MultiServer.py:594-598`
/// sets them to the goal, and `update_client_status` never changes a goal, `2316-2317`): their
/// watchers hear nothing when they join or leave, and `Get` reads 30.
#[tokio::test]
async fn a_spectator_publishes_no_status_and_reads_as_done() {
    let room = room_edit(|v| v["slots"][1]["kind"] = json!("spectator"));
    let mut a = joined(&room, "A").await;
    watch(&mut a, &["_read_client_status_0_2"]).await;
    let mut b = joined(&room, "B").await;
    send(&mut b, &json!([{"cmd": "StatusUpdate", "status": 20}])).await;
    b.leave().await;
    assert_eq!(set_reply_of(&pump(&mut a).await), Vec::<&Value>::new());
    assert_eq!(get(&mut a, &["_read_client_status_0_2"]).await["_read_client_status_0_2"], 30);
}

/// Data storage writes cost their stored bytes from a per-slot bucket (adversarial review on
/// PR 46: every write logs its whole value). A slot past its budget is refused with the
/// registry's code; other slots and chat are not affected.
#[tokio::test]
async fn set_bytes_are_budgeted_per_slot_with_the_registry_code() {
    let room = limited(&Limits {
        kv_write_bytes_burst: 100,
        kv_write_bytes_refill_milli_per_sec: 0,
        ..Limits::DESIGN
    });
    let mut a = joined(&room, "A").await;
    // "k" plus a 50-character string with its quotes: 53 bytes per write.
    let p = set("k", &json!([{"operation": "replace", "value": "x".repeat(50)}]));
    assert_eq!(send(&mut a, &p).await, Vec::<Value>::new());
    assert_eq!(send(&mut a, &p).await, Vec::<Value>::new(), "47 bytes left: allowed, in debt");
    let refused = send(&mut a, &p).await;
    let text = refused[0]["text"].as_str().unwrap();
    assert!(text.contains("room.write_rate") && text.contains("3004"), "{text}");
    assert_eq!(send(&mut a, &json!([{"cmd": "Say", "text": "hi"}])).await, Vec::<Value>::new());
    let mut b = joined(&room, "B").await;
    assert_eq!(send(&mut b, &p).await, Vec::<Value>::new());
}

/// The room's total of logged data storage bytes is capped (until the log is compacted):
/// past it a write is refused with `room.storage_full`.
#[tokio::test]
async fn the_rooms_logged_storage_bytes_are_capped_with_the_registry_code() {
    let room = limited(&Limits { kv_max_room_log_bytes: 20, ..Limits::DESIGN });
    let mut a = joined(&room, "A").await;
    let p = set("k", &json!([{"operation": "replace", "value": "12345"}])); // 8 bytes
    assert_eq!(send(&mut a, &p).await, Vec::<Value>::new());
    assert_eq!(send(&mut a, &p).await, Vec::<Value>::new());
    let text = send(&mut a, &p).await[0]["text"].as_str().unwrap().to_owned();
    assert!(text.contains("room.storage_full") && text.contains("3003"), "{text}");
}

mod hints;
// The host's room view (view.rs): numbers from the room and its sessions, events as shown.

fn slot(summary: &Value, id: u64) -> &Value {
    summary["slots"].as_array().unwrap().iter().find(|s| s["slot"] == id).unwrap()
}

#[tokio::test]
async fn the_summary_counts_real_sessions_checks_and_items() {
    let room = room();
    let fresh = room.summary().await.unwrap();
    assert_eq!(fresh["seed"], "seed-1");
    assert_eq!(
        fresh["options"],
        json!({"password": false, "hint_cost": 10,
        "release_mode": "auto", "collect_mode": "auto"})
    );
    assert_eq!(
        *slot(&fresh, 1),
        json!({"slot": 1, "team": 0, "name": "A", "game": GAME, "connected": false, "clients": 0,
               "checked": 0, "locations": 2, "items_received": 0, "goal": false})
    );
    let mut a = joined(&room, "A").await;
    let mut a2 = joined(&room, "A").await;
    let now = room.summary().await.unwrap();
    assert_eq!((&slot(&now, 1)["connected"], &slot(&now, 1)["clients"]), (&json!(true), &json!(2)));
    assert_eq!(slot(&now, 2)["connected"], false, "B has no session");
    check(&mut a, &[100]).await;
    let now = room.summary().await.unwrap();
    assert_eq!(
        (&slot(&now, 1)["checked"], &slot(&now, 2)["items_received"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(slot(&now, 1)["items_received"], 0, "the item went to B");
    a.leave().await;
    assert_eq!(slot(&room.summary().await.unwrap(), 1)["clients"], 1);
    a2.leave().await;
    assert_eq!(slot(&room.summary().await.unwrap(), 1)["connected"], false);
}

#[tokio::test]
async fn the_summary_says_only_whether_a_password_is_set() {
    let s = room_with(&json!("hunter2"), None).summary().await.unwrap();
    assert_eq!(s["options"]["password"], true);
    assert!(!s.to_string().contains("hunter2"));
}

#[tokio::test]
async fn durable_events_are_described_with_names_and_data_writes_are_hidden() {
    let room = room();
    let mut a = joined(&room, "A").await;
    check(&mut a, &[100, 101]).await;
    send(&mut a, &json!([{"cmd": "Set", "key": "k", "default": 0, "operations": []}])).await;
    let history = room.watch(proto::Seq(0), 400).await.unwrap().0.history;
    let shown: Vec<Value> = history.iter().filter_map(|e| room.describe_event(e)).collect();
    assert_eq!(history.len(), 5, "two checks, two items, one write: {history:?}");
    assert_eq!(
        shown,
        [
            json!({"kind": "checked", "seq": 1, "slot": 1, "name": "A", "location": 100,
                   "location_name": "L100"}),
            json!({"kind": "item", "seq": 2, "from": 1, "from_name": "A", "to": 2, "to_name": "B",
                   "item": 7, "item_name": "Seven", "location": 100, "location_name": "L100",
                   "flags": 1}),
            json!({"kind": "checked", "seq": 3, "slot": 1, "name": "A", "location": 101,
                   "location_name": "L101"}),
            json!({"kind": "item", "seq": 4, "from": 1, "from_name": "A", "to": 1, "to_name": "A",
                   "item": 8, "item_name": "Eight", "location": 101, "location_name": "L101",
                   "flags": 0}),
        ]
    );
}

#[tokio::test]
async fn joins_chat_and_leaves_are_described_from_the_live_stream() {
    let room = room();
    let (sub, _, _) = room.watch(proto::Seq(0), 400).await.unwrap();
    let mut live = sub.live;
    let mut a = joined(&room, "A").await;
    send(&mut a, &json!([{"cmd": "Say", "text": "hello"}])).await;
    a.leave().await;
    let mut seen = Vec::new();
    while let Ok(p) = live.try_recv() {
        seen.extend(room.describe(&p));
    }
    let kinds: Vec<(&str, &Value)> =
        seen.iter().map(|e| (e["kind"].as_str().unwrap(), &e["text"])).collect();
    assert_eq!(
        kinds,
        [("join", &Value::Null), ("chat", &json!("hello")), ("leave", &Value::Null)],
        "{seen:?}"
    );
    assert!(seen.iter().all(|e| e["slot"] == 1 && e["name"] == "A"), "{seen:?}");
}

#[test]
fn a_view_starts_after_its_cursor_unless_it_is_past_the_head_or_too_far_behind() {
    use proto::Seq;
    // (after, head, cap) -> (start, reset)
    for ((after, head, cap), want) in [
        ((0, 0, 400), (0, false)),
        ((0, 50, 400), (0, false)),
        ((0, 1000, 400), (600, false)),
        ((40, 50, 400), (40, false)),
        ((50, 50, 400), (50, false)),
        ((57, 50, 400), (0, true)),
        ((1057, 1000, 400), (600, true)),
        ((100, 1000, 400), (600, true)),
        ((600, 1000, 400), (600, false)),
        ((5, 9, 0), (9, true)),
    ] {
        let got = crate::view::view_start(Seq(after), Seq(head), cap);
        assert_eq!((got.0.0, got.1), want, "after {after}, head {head}, cap {cap}");
    }
}
