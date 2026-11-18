# Protocol plan: the next compat features

Internal working note, started 2026-11-16, kept current as the features land. Our own words. Sources read at the Archipelago 0.6.8 checkout pinned by Archipela-Go:
- "protocol doc" = `Archipela-Go/.ap/docs/network protocol.md` (cited by section name)
- "reference server" = `Archipela-Go/.ap/MultiServer.py` and `NetUtils.py` (cited `file:line`; behavior read from code, not run, unless marked)
- "ours" = `constellation-server/crates/...`

This note is our own writing, with citations; it holds no Archipelago source code, text or assets. "Read from code" means we traced it by reading, not by running it. Every limit we propose is an estimate, labeled as such, and belongs in `crates/config`.

## Status (2026-11-17)

Build order steps 1 to 3 (section 7) are implemented on `feat/data-storage`; `docs/compat-status.md` lists what is supported and the test that proves each part. Choices made while building, beyond what is written below:
- Step 1 imports only what delivery and data storage need now: starting inventory, item and location name groups, and the generic `Archipelago` package. Start hints, entrances, spheres and hint blacklists come with hints (step 4).
- `room-core` has no float math library (`no_std`), so the host supplies float power in `Ctx::powf`, like the clock; the result is recorded in the event, so replay never needs it.
- Section 0.6 (a connection id) was not needed: the room's reply to a `Set` carries the event's `seq`, and the writing session skips that `seq` when the event reaches it, live or after a resync. Notes (section 0.5) are `runtime::Note { seq, reply, extra }`.
- Client status presence lives in `edge-compat` (`presence.rs`) under a lock held while each change is published, so watchers see changes in order.
- Found by running Archipelago's own `CommonClient` against the server: the reference tags `NetworkItem`, `NetworkPlayer` and `NetworkSlot` with `"class"` (`NetUtils.py:98-107`) and its client only builds them from tagged objects; we tag them now.
---

## 0. Cross-cutting changes (do these first, every feature below needs them)

1. **`proto::Command` and `proto::Event` stop being `Copy`** (`crates/proto/src/msg.rs:10,40,51`). KV values, hint lists and key strings are heap data. `Arc<[Event]>` in `runtime::Published` already avoids clones on fan-out.
2. **Room options into room-core.** `RoomState::new` (`room-core/src/lib.rs:183`) takes a `RoomOptions { hint_cost_pct, location_check_points, release, collect, remaining, groups: BTreeMap<SlotNo, Vec<SlotNo>> }`. Placement value grows from `(receiver, item)` to `(receiver, item, flags, sphere)` so room-core can pick hints (trap flag, sphere order) without asking the edge.
3. **`validate()` head rule** (`room-core/src/lib.rs:250`, "two events per check plus one per goal") must become a per-kind count, or every new event kind breaks snapshot validation.
4. **Read queries.** Add `RoomMsg::Read` to the runtime actor (`runtime/src/lib.rs`): a read-only query answered from `RoomState`, no log write. Used by `Get` (KV keys, `_read_hints_*`, `_read_client_status_*`). Ordering note: the reply can overtake published events still in the session queue; this is safe because every later `SetReply` carries the full value, so the client converges on the newest value.
5. **Non-durable annotations on a published batch.** Doc 05 7.1 sketches `Applied.effects`; we need it now. `Published::Events` gains an optional `notes: Arc<[Note]>` (KV original value and pass-through fields, the origin session, rechecked hint lists per changed slot). Notes ride in the same queue entry as the events. A session that resyncs from the log (`Session::resync`) gets the events without notes and must degrade as stated per feature.
6. **Session identity.** Each `Session` gets a process-unique `ConnId` (u64 counter) so it can recognise its own writes in published notes.

---

## 1. Data storage: `Get`, `Set`, `SetNotify`, `SetReply`

### 1.1 Packets and fields

| Packet | Fields | Notes |
|---|---|---|
| `Get` (in) | `keys: list[str]`, plus any extra fields | protocol doc "Get"; reference `MultiServer.py:2271-2283` |
| `Retrieved` (out) | `keys: dict[str, any]`, plus every extra field of the `Get` echoed | missing key reads `null` (doc "Retrieved") |
| `Set` (in) | `key: str`, `default: any` (optional), `want_reply: bool` (optional, false), `operations: list[{operation: str, value: any}]`, plus extras | doc "Set", "DataStorageOperation"; reference `MultiServer.py:2285-2304` |
| `SetReply` (out) | `key`, `value` (new), `original_value` (before), `slot` (setter), plus every extra field of the `Set`, including `want_reply`, `default` and `operations` themselves | reference reuses the incoming dict (`MultiServer.py:2291-2303`), so all request fields are echoed |
| `SetNotify` (in) | `keys: list[str]` | registers this connection only; sends nothing back (`MultiServer.py:2306-2312`). Clients `Get` first, then `SetNotify` (reference client, `CommonClient.py:1056-1061`) |

Reserved read keys (`_read_` prefix, `Get` only; the prefix is stripped and looked up in a table built at load, `MultiServer.py:572-575, 583, 609-612`, `2279`):

| Key | Value | Ours today (`edge-compat/src/session.rs:557-576`) | Change notification |
|---|---|---|---|
| `_read_hints_{team}_{slot}` | list of Hint objects for that slot as finder or receiver, rechecked for `found` | `[]` | yes, `SetReply` on new or changed hints (`MultiServer.py:962-966`) |
| `_read_slot_data_{slot}` | that slot's `slot_data` (any slot, no ownership check) | done | never |
| `_read_item_name_groups_{game}` | dict group name to list of item names | `null` (missing) | never |
| `_read_location_name_groups_{game}` | dict group name to list of location names | `null` (missing) | never |
| `_read_client_status_{team}_{slot}` | int ClientStatus | 0 or 30 only | yes (`MultiServer.py:968-972`) |
| `_read_race_mode` | 0 or 1 | done | never |
| any other `_read_*` | `null` | `null` | never |

`SetReply` for `_read_` keys carries only `cmd`, `key`, `value`: no `original_value`, no `slot`, no extras (`MultiServer.py:966, 972`). `archipelago_rs` treats both as optional (`Archipela-Go/core/vendor/archipelago_rs/src/protocol.rs:546`, `event.rs:121`).

### 1.2 Set semantics (reference `MultiServer.py:174-197` table, `127-155` helpers, `2292-2298`)

Start value = stored value, else the packet's `default`, else **0** when `default` is absent. Apply each operation in order to a working copy. Store the result (the key is written even when nothing changed, and even with an empty `operations` list). Reply rules below. Exact per-operation behavior we must match, in Python terms because the reference evaluates with Python operators:

| op | current, value | result | refused (InvalidPacket for us; the reference raises and drops the connection, see 1.6) |
|---|---|---|---|
| `replace` | any | value | |
| `default` | any | current unchanged (the start value already applied the default) | |
| `add` | num+num | sum (int+int stays int, any float gives float, bool counts as 0/1) | |
| | str+str | concatenation | |
| | list+list | concatenation | |
| | other mixes, dict+dict | | yes |
| `mul` | num*num | product | |
| | str*int, list*int (either order) | repetition; int <= 0 gives empty | yes when result exceeds the value cap |
| `pow` | num**num | power; int to a negative int gives a float; 0 to a negative power refused | yes on overflow |
| `mod` | num%num | floor-mod: result takes the divisor's sign (Python rule, not Rust `%`) | divisor 0 refused |
| `floor`, `ceil` | num | int (a float becomes an int; int unchanged). `value` key must still be present (see disagreements) | non-number refused |
| `max`, `min` | comparable pair | the larger/smaller; **on a tie the current value wins** (Python returns the first argument) so `max(1, 1.0)` keeps the int | number vs str etc. refused; str vs str and list vs list compare lexicographically |
| `and`, `or`, `xor` | int,int | bitwise; bool,bool gives bool | float refused |
| | dict `or` dict | merge, right side wins (Python dict union) | dict `and`/`xor` refused |
| `left_shift`, `right_shift` | int,int | shift; right shift of a negative is arithmetic | negative shift refused; left shift overflow refused |
| `remove` | list, any | first element equal to value removed; absent is a no-op. Equality is Python's: `1 == 1.0 == true` | non-list refused |
| `pop` | list, int | removes that index; negative counts from the end; index >= len is a no-op; index < -len refused (Python IndexError). A bool index counts as 0/1 | list with non-int refused |
| | dict, any | removes that key if present, else no-op | |
| `update` | list, list | appends each entry not already present, in order | |
| | list, dict | appends the dict's keys not present (Python iterates keys) | |
| | list, str | appends each character not present (Python iterates characters) | |
| | dict, dict | merge, value side wins | |
| | dict, list of 2-item lists | merge pairs | other shapes refused |
| unknown op name | | | refused |

Number model for us (deviation, labeled): Python integers are unbounded; serde_json holds i64/u64/f64. Use i64 with checked arithmetic, f64 for floats, refuse any result that overflows i64 or is NaN/infinite. Risk: a client that relies on integers above 2^63 (EnergyLink-style counters are the likeliest; real magnitudes **not verified**) gets InvalidPacket instead of a value. Integers above 2^53 also lose precision in many JavaScript clients anyway.

All-or-nothing for us: if any operation is refused, nothing is stored and nothing is sent except the InvalidPacket. (The reference can partially mutate the stored list in place before a later op fails, `MultiServer.py:2292-2298` with the in-place helpers at `127-155`; we do not copy that.)

### 1.3 Who gets a SetReply (reference `MultiServer.py:2299-2303`)

Targets = every connection that sent `SetNotify` for exactly this key, plus the setter if `want_reply` is true. A set, so the setter gets one copy even if it is also subscribed. Sent even when the value did not change (doc "SetReply"). Subscriptions are per connection, not durable, and lost on disconnect (`MultiServer.py:396` uses a weak set). Any authenticated client may set any non-reserved key, including keys other slots use; there is no ownership (parity; note as a trust gap for the native protocol, not for compat).

### 1.4 State and durability

- `RoomState.kv: BTreeMap<String, Value>` in room-core, part of the snapshot. Durable, because the reference saves it (`MultiServer.py:760, 807-808`) and clients use it as save data (for example per-slot progress keys).
- New `Command::KvSet { slot, key, default, ops: Vec<KvOp> }` (KvOp is our own enum; the edge maps the wire names onto it). room-core evaluates the ops in `apply` (pure, deterministic) and emits **`EventKind::KvSet { key, value }`** carrying the resulting value, so `fold` never re-evaluates operations (no float or library drift on replay).
- `Reply::KvSet { original, value }` back to the setter.
- Note to publish: `{ origin: ConnId, original, extras }`. The session that owns `origin` skips the event-driven copy (it already answered from the reply). On resync without notes: subscribers get `SetReply` with `value` and `slot`, `original_value: null`, no extras (deviation, only after a queue overflow).
- room-core needs `serde_json` with `default-features = false, features = ["alloc"]` (no_std stays intact).
- **SetNotify watch set** lives in the session (`Joined.watch: BTreeSet<String>`). Not durable.
- Cost: every KV write is one log append and is published to every session (each session filters by its watch set). Name it: EnergyLink-style keys can be written several times a second per player. Batching in the runtime already folds a mailbox message into one append; no per-key coalescing.

### 1.5 Limits (estimates, all to `crates/config`)

- key length <= 1 KiB; `_read_` prefix refused for `Set`.
- stored value <= 64 KiB serialized; whole room KV <= 16 MiB; refuse the `Set` that would cross either.
- operations per `Set` <= 64; keys per `Get` and per `SetNotify` <= 1 024; watched keys per session <= 4 096.
- JSON nesting depth: the reference rejects any frame nested deeper than 16 (`NetUtils.py:179-205`). Apply the same depth cap to `Set` values (serde_json alone allows 128).
- `Set` rate per slot: own `RateTable` bucket (estimate burst 50, 20 per second), separate from chat so EnergyLink traffic does not mute chat.
- `mul`/`left_shift`/`pow` checked before allocating (a list times 10^9 must be refused, not built).

### 1.6 Edge cases

- `Get` with a non-list `keys`: InvalidPacket `arguments`. A non-string entry: the reference crashes the connection (calls a string method on it, `MultiServer.py:2279`); we refuse with InvalidPacket.
- `Get` and `Set` before `Connect`: InvalidPacket `cmd` (as today, `session.rs:226-228`).
- `Set` on a key just read by another client in the same batch: the room actor serialises, so last writer wins in mailbox order.
- Group, spectator and tracker sessions may `Get`/`Set`/`SetNotify` (reference has no check).

### 1.7 Owners

`edge-compat` (wire, `_read_` table, watch set, SetReply building), `room-core` (KV map, op evaluation, KvSet event, limits check), `runtime` (Read query, notes on Published), `config` (limits).

### 1.8 Scenarios

1. `a_set_with_add_replies_and_notifies_subscribers`
2. `a_set_without_want_reply_and_no_subscribers_sends_nothing`
3. `a_setter_that_also_subscribes_gets_one_set_reply`
4. `set_reply_echoes_every_extra_field_of_the_set`
5. `a_set_on_a_missing_key_starts_from_default_and_without_default_from_zero`
6. `a_set_with_no_operations_still_stores_the_default`
7. `every_operation_matches_the_reference_on_its_table` (table test in room-core over 1.2, including floor-mod signs, max tie keeps current, update on list with dict and string, pop negative index)
8. `a_refused_operation_changes_nothing_and_the_connection_stays_open`
9. `a_reserved_key_cannot_be_set`
10. `a_value_over_the_cap_and_a_runaway_multiply_are_refused`
11. `stored_values_survive_a_server_restart` (MemLogStore fold; Postgres later)
12. `get_returns_null_for_unknown_keys_and_echoes_extra_fields`
13. `name_groups_are_readable_for_every_game_in_the_room`
14. `subscriptions_end_with_the_connection_and_a_reconnect_must_subscribe_again`
15. `a_subscriber_that_fell_behind_still_converges_on_the_latest_value`
16. `client_status_changes_reach_subscribers_of_the_status_key`
17. `set_is_rate_limited_per_slot_without_muting_chat`

---

## 2. Hints

### 2.1 Packets and fields

| Packet | Fields | Behavior |
|---|---|---|
| `LocationScouts` (in) | `locations: list[int]`, `create_as_hint: int` (0, 1, 2; the reference casts with `int()`, so `true` means 1) | always answers `LocationInfo` with one NetworkItem per requested id (duplicates kept), `player` = receiver (`MultiServer.py:2086-2105`). With `create_as_hint` != 0: each location becomes a hint at status UNSPECIFIED (FOUND if already checked), stored **even if already found**, free (no points). With 2, only hints that did not exist before are announced; with 1, existing ones are announced again (`MultiServer.py:895-896, 2102`). |
| `CreateHints` (in) | `locations: list[int]` (non-empty), `player: int` (default: own slot), `status: int` (default 0) | rules at `MultiServer.py:2107-2157`: for another slot's world every location must exist and hold an item for the sender (or a group containing the sender); for items going to others the status must be UNSPECIFIED and the location must be the sender's own. Existing hints are skipped without status change. Free. |
| `UpdateHint` (in) | `player: int` (finding slot), `location: int`, `status: int` | only a receiver of the hint (or a member of the receiving group) may change it; unknown hint is ignored silently; FOUND cannot be set; a found hint stays FOUND; no PrintJSON, only `_read_hints` SetReply to both sides (`MultiServer.py:2159-2201`). |
| `Say "!hint <item or item group>"`, `"!hint_location <location or location group>"`, bare `"!hint"` | chat text | section 2.4 |
| `PrintJSON` type `Hint` (out) | `data` parts, `receiving`, `item` (NetworkItem with `player` = finder), `found` | built per hint (`NetUtils.py:421-442`); parts: text, `player_id`, `item_id` (with receiver and flags), `location_id` (with finder), optional `entrance_name`, and a `hint_status` part (fields `text`, `hint_status`) |
| `RoomUpdate.hint_points` (out) | int | to every connection of each slot that got a new hint (`MultiServer.py:955-960`) and of the checking slot after checks (`1254-1258`) |
| `SetReply` `_read_hints_{team}_{slot}` | full list | on new hints, status changes, and when a check flips `found` (`MultiServer.py:1259-1262`) |

Hint object on the wire (protocol doc "Hint"; the reference adds `"class": "Hint"` when encoding, `NetUtils.py:98-104`): `receiving_player, finding_player, location, item, found, entrance, item_flags, status`. Status values (doc "HintStatus"): 0 unspecified, 10 no priority, 20 avoid, 30 priority, 40 found.

Who sees a Hint print (`MultiServer.py:892-931`): the receiving slot (every group member when the receiver is a group) and the finding slot when it differs; every non-NoText connection of those slots. Not everyone. Within one slot's batch, hints where that slot is the finder come first.

### 2.2 Economy (protocol doc "RoomInfo" `hint_cost`, `location_check_points`; reference `MultiServer.py:816-819, 1929-1936`)

- cost per hint for slot S = 0 when `hint_cost` is 0, else max(1, floor(hint_cost% x number of locations in S's own world)).
- points(S) = location_check_points x checked locations in S's world - cost(S) x hints_used(S).
- Checked locations include those checked by release and by other players' collect, so a collect by someone else raises your points.
- Only `!hint` and `!hint_location` spend points. Scouts and CreateHints are free.
- Ours today: `hint_points` ignores spending (`session.rs:652-656`).

### 2.3 State and durability

- Durable, as room-core events (the reference saves `hints` and `hints_used`, `MultiServer.py:749-750`):
  - `EventKind::HintAdded { finding: SlotNo, location: i64, status: u8, paid: bool }` (slot = requester). Item, receiver, flags derive from the placement; entrance from the bundle. Unique per (finding, location).
  - `EventKind::HintStatus { finding, location, status }`.
- Derived, not stored: `found` = location checked; effective status = FOUND when found, else stored status (matches `NetUtils.py:403-416`). Receivers expand through groups.
- Seed data in the bundle (import from multidata, `Main.py:273-291, 248-249, 324-348`): `start_hints` per slot (precollected hints with their status), `entrances` (er_hint_data), `spheres` (for ordering), `hint_blacklist` per game (from each world class, `worlds/AutoWorld.py:312`; not in the multidata, so the importer must read it from the loaded world; **inferred**).
- `RoomState.hints: BTreeMap<(SlotNo, i64), HintRec>` and `hints_used: BTreeMap<SlotNo, u32>` in the snapshot.
- Commands: `HintLocations { slot, finding, locations, status, persist_found: bool, only_new: bool, pay: bool }` covers scouts, CreateHints and `!hint_location`; `HintItems { slot, item_ids }` covers `!hint` (room-core finds placements whose receiver is the slot or a group containing it, `MultiServer.py:1266-1302`). Reply returns the hints to print and the payment outcome, so the edge writes the CommandResult text.
- Prints for new hints follow from durable `HintAdded` events (every session decides if it is concerned). Re-announced old hints (scouts with 1, `!hint` on an already-known hint) are not events: fan out through `Published::Ext` with a target slot list.
- `_read_hints` notifications: the runtime attaches the rechecked list for each slot whose hints changed in the batch (note, section 0.5). On resync a watching session issues one Read for its watched hint keys.

### 2.4 `!` chat commands

The protocol doc defines no packet for item hints; `!hint` exists only as chat text handled by the server (`MultiServer.py:1503-1918`). Clients do not require it to connect, but players and the reference text client use it (the client autocompletes from the exact reply wording "did you mean '...'?", `Utils.py:723-742`, `kvui.py:450`), so it is required for parity.

Parsing (`MultiServer.py:1409-1442`): text starting with `!`, command name case-insensitive, argument is the raw rest of the line for hint commands. **Every `!` text except `!admin` is first echoed to everyone as an ordinary Chat print** (`MultiServer.py:1510-1514`); ours does not echo today (`session.rs:494-501`). Replies are `PrintJSON` type `CommandResult` to the sender only.

`!hint` with text (`MultiServer.py:1775-1904`):
1. Resolve the name against the sender's game: item names plus item group names (locations and location groups for `!hint_location`), with fuzzy matching. The reference ranks by Damerau-Levenshtein ratio, accepts an exact or case-insensitive match, accepts a best score >= 75 that beats the runner-up by more than 5, and otherwise answers with a "did you mean" suggestion (`Utils.py:676-720`). A numeric argument is taken as an id. Names in the world's hint blacklist answer "non-hintable".
2. Candidates: every placement of that item for the sender (or its groups). Already-known hints are shown again free. Candidates already found at hint time are shown free and **not stored** for `!hint` (they are stored for scouts).
3. Payment: when cost > 0, at most one new unfound hint per command, and only if points >= cost; when cost is 0, all of them. Each paid hint adds one to hints_used.
4. Choice among unfound candidates: earliest sphere first; within a sphere, prefer a location in another player's world; ties settled by the room's seeded random (`MultiServer.py:1858-1871`). For us: room-core picks with `spec-rng` seeded from (seed name, slot, hints_used), deterministic, and the pick is recorded in the event so fold never re-picks.
5. Status: FOUND if found; otherwise AVOID when the item is a trap, else PRIORITY (`MultiServer.py:1289-1296`). `!hint_location` uses UNSPECIFIED.
6. Bare `!hint`: resend your known hints to you only, then "a hint costs X, you have Y".

Other commands to implement now: `!help`, `!remaining` (section 3), `!release`, `!collect` (section 3, policy question), `!missing`, `!checked` (with group or substring filter, `MultiServer.py:1679-1729`), `!players`, `!status`. Later: `!countdown`, `!alias` (durable alias plus `RoomUpdate.players`). Never: `!admin` (doc 10 section 8.2), `!getitem` (item_cheat is not imported). `!options` in the reference prints the room password to the asking client (its own help text warns of this, `MultiServer.py:1493-1500`); if we add it, mask the password.

### 2.5 Edge cases and limits

- `LocationScouts` with a location outside the world: ours answers InvalidPacket (`session.rs:447-468`); the reference raises KeyError and the connection ends (`MultiServer.py:2098`). Keep ours.
- `create_as_hint` from trackers is allowed in the reference (no check). Parity.
- Limits (estimates): locations per scout or CreateHints packet <= the world size (refuse more); hint-creating packets share a per-slot bucket (burst 10, 1 per second) because each creates durable events; `!` commands share the chat bucket (as today).
- Fuzzy matching cost: Damerau-Levenshtein over every name of a large game (tens of thousands of names, **not verified** for any specific game) per `!hint`; cap input to 128 bytes and rate-limit.
- Hint printing for groups: one print per concerned slot, not per member duplication.

### 2.6 Owners

`edge-compat` (wire, `!` parser, fuzzy names, prints, CommandResult text, `_read_hints` key), `room-core` (hint table, economy, selection, events), `seed-bundle` and `tools/ap-import` (start hints, entrances, spheres, blacklist, name groups), `spec-rng` (selection stream), `config` (limits).

### 2.7 Scenarios

1. `a_scout_with_create_as_hint_stores_a_free_hint_and_tells_both_players`
2. `a_scout_with_create_as_hint_two_announces_only_new_hints`
3. `a_scout_of_a_found_location_still_stores_a_found_hint`
4. `create_hints_for_another_players_location_needs_an_item_for_you`
5. `create_hints_skips_existing_hints_without_changing_their_status`
6. `update_hint_by_the_receiver_changes_status_and_notifies_both_sides`
7. `update_hint_by_anyone_else_is_refused_and_found_cannot_be_set`
8. `hint_points_grow_with_checks_and_drop_by_the_cost_per_paid_hint`
9. `bang_hint_pays_for_one_new_hint_per_command_when_hints_cost_points`
10. `bang_hint_is_free_and_returns_all_when_hint_cost_is_zero`
11. `bang_hint_on_a_known_hint_shows_it_again_and_charges_nothing`
12. `bang_hint_prefers_the_earliest_sphere_then_another_players_world`
13. `bang_hint_with_a_misspelled_name_suggests_the_closest_name`
14. `bang_hint_on_a_blacklisted_item_says_it_is_not_hintable`
15. `bang_hint_with_too_few_points_says_what_it_costs`
16. `bang_commands_are_echoed_to_everyone_as_chat`
17. `a_check_marks_a_hint_found_and_notifies_hint_subscribers`
18. `hint_prints_reach_only_the_finder_and_the_receiver`
19. `start_hints_from_the_seed_are_readable_on_first_connect`
20. `hints_and_points_survive_a_server_restart`

---

## 3. Release, collect, remaining

### 3.1 Modes (protocol doc "RoomInfo" release/collect/remaining, "Permission"; reference `NetUtils.py:55-71`)

`RoomInfo.permissions` values: disabled 0, enabled 1, goal 2, auto 6, auto-enabled 7. `remaining` only 0, 1, 2. Ours today advertises all 0 (`session.rs:177-179`) and the bundle already carries the modes (`seed-bundle/src/lib.rs` `ServerOptions`).

| Mode | Manual `!release` / `!collect` | On goal |
|---|---|---|
| disabled | refused ("ask the host") | nothing |
| enabled | any time | nothing |
| goal | only after the slot's goal | nothing |
| auto | after the goal (reference allows it: the goal bit is part of 6, `MultiServer.py:1605-1613, 1624-1632`) | automatic |
| auto-enabled | any time | automatic |

`remaining`: enabled any time, goal after goal, disabled refused (`MultiServer.py:1651-1677`). The reference also has a per-slot host override that allows `!release` any time (`allow_releases`, `MultiServer.py:1595, 2457-2468`) and host console `/release`, `/collect` for any slot (`2420-2455`).

### 3.2 What each does

- **Release** of slot S (`MultiServer.py:1176-1183`): a `Release` PrintJSON to everyone (all teams; fields `team`, `slot`), then every unchecked location of S's world is checked as if S checked it: items go to their receivers with normal ItemSend prints, ReceivedItems, `RoomUpdate` (new checks, hint points) to S, hints rechecked; then a second `RoomUpdate` with S's full checked list. Release with nothing left still prints.
- **Collect** for slot S (`MultiServer.py:1186-1203`): a `Collect` PrintJSON to everyone, then for every world W holding an item for S at an unchecked location, those locations are checked in W (as W's checks, without touching W's activity timer). S gets its items; W's clients get `RoomUpdate` with those locations as checked (W's missing list shrinks, W's hint points rise). Own-world items for S are included. Group items: once every member of a group has collected, the group's items are collected too (tracked durably, `group_collected`, `MultiServer.py:759, 804-805`).
- **Auto on goal** (`MultiServer.py:945-953`): Goal print, then collect if collect mode contains auto, then release if release mode contains auto. In that order.
- **Remaining**: CommandResult listing item names (receiver's game) at unchecked locations in your world, without locations or receivers (`NetUtils.py:500-506`). Read only.

### 3.3 Policy conflict with our design (owner decision)

Doc 05 7.1 puts release and collect under `Command::Host(HostAction)` ("never a slot token"), and doc 10 sections 8.2 item 4 and 7 say compat clients never trigger release or collect and that manual release needs step-up in the web console, while a room may pre-authorize auto-release on goal (doc 10 line 191). The reference lets the slot's own client run `!release` / `!collect` when the mode allows.

Recommended default (reversible): honor `auto` on goal (pre-authorized by the seed's mode), answer `!release` / `!collect` from compat clients with a CommandResult pointing to the host console, implement `!remaining` per mode (read only), and **advertise only what we honor** in `RoomInfo.permissions`: release/collect auto-enabled and auto become auto (6), enabled and goal become disabled (0). The one question for the owner: allow player-triggered `!release`/`!collect` from compat clients when the seed allows it (parity), or keep doc 10's rule?

### 3.4 State and durability

- New commands: `Release { slot, by: Auto | Host | Player }`, `Collect { slot, by }`. Auto ones are emitted inside `apply(Goal)` in the same command, so the goal and its automatic release/collect are one durable batch.
- Events: `EventKind::Released` and `EventKind::Collected` markers (slot = S), then ordinary `Checked`/`Item` pairs. `Checked` needs `cid: Option<Cid>` (None for server-originated checks; skip the dedupe insert). A later client resend of a released location is `AlreadyChecked` through `checked_at` as today.
- Group collect state derives from `Collected` markers (a group is collected when all members have one), emitted in the same apply as the last member's collect.
- Marker events let the edge print Release/Collect before the item prints, matching reference order.
- Size: releasing a 1 000-location world is about 2 000 events in one append (estimate from the two-events-per-check rule). Fine for MemLogStore; for Postgres (D-98) check batch insert size.

### 3.5 Edge cases

- Release/collect from a tracker or text client: refused (they cannot check).
- Goal arriving twice: second is answered from the first (exists today), so auto release runs once.
- Collect when the slot has nothing outstanding: Collect print, no checks.
- Collect never checks locations whose item belongs to a group that has not fully collected.
- `remaining` permission auto or auto-enabled is refused by the bundle already (`seed-bundle/src/lib.rs:226-229`).

### 3.6 Owners

`room-core` (commands, markers, auto on goal, group collect), `edge-compat` (permissions in RoomInfo, `!` commands, prints, RoomUpdate), `seed-bundle` (modes, done), host console path later (doc 10).

### 3.7 Scenarios

1. `room_info_advertises_the_permissions_the_server_honors`
2. `auto_release_on_goal_sends_every_remaining_item_to_its_receiver`
3. `auto_collect_on_goal_checks_the_slots_items_in_other_worlds_first`
4. `a_collected_location_shows_as_checked_to_the_world_that_held_it`
5. `collect_raises_the_hint_points_of_the_worlds_it_checks`
6. `release_and_collect_print_to_every_player`
7. `a_released_location_resent_by_the_client_delivers_nothing_twice`
8. `bang_release_in_goal_mode_is_refused_before_the_goal` (and the default-policy variant `bang_release_from_a_compat_client_points_to_the_host_console`)
9. `bang_remaining_lists_item_names_only_when_the_mode_allows`
10. `a_goal_and_its_auto_release_survive_a_restart_as_one_batch`
11. `a_group_is_collected_only_after_every_member_collects`

---

## 4. Starting inventory and group slots

### 4.1 Starting inventory (protocol doc "items_handling flags" bit 0b100, "Data Package Contents" location -2; reference `MultiServer.py:587-589, 1146-1168, 2014-2018`)

- Each start item is a NetworkItem `{item, location: -2, player: 0, flags: 0}` (flags default 0, `NetUtils.py:90-95`).
- Sent only to clients with bit 2 (which needs bit 0). **They come first in the index space**: index 0..k-1 are the start items, then received items. A client without bit 2 sees received items from index 0. So the same slot has two index spaces depending on the flag.
- No PrintJSON for start items. Sent on Connect, Sync and items_handling change (all from index 0), like today.
- State: fixed seed data, not durable events. `SeedBundle.slots[].start_inventory: Vec<i64>`; the importer stops refusing (`tools/ap-import/ap_import.py:70-72`) and copies `precollected_items` (item ids only, `Main.py:273-274`).
- Edge: `Handling` gains `start: bool` (`session.rs:46-69` currently ignores bit 2); `Joined::wanted()` prepends start items when `start`; `sent` counts them, so live items continue at k + n.
- Clients name location -2 and player 0 from the generic `Archipelago` game package (protocol doc "Data Package Contents"; the reference client maps player 0 to that game, `CommonClient.py:1038`). The multidata embeds that package (`Main.py:320`) but the importer drops it (`ap_import.py:98`). Keep it, list `Archipelago` in `RoomInfo.games` and checksums, and serve it from `GetDataPackage` (reference always includes it, `MultiServer.py:559, 1012`).

### 4.2 Group slots (item links) (protocol doc "SlotType", "NetworkSlot"; reference `MultiServer.py:560-561, 594-598, 862-865, 1210-1215, 1197-1203, 1274-1277`)

- A group slot has `type` 2 and `group_members`. It has no locations of its own (the generator only builds location tables for players, `Main.py:292`). It counts as having reached its goal from the start (`NetUtils.py:49-52`, `MultiServer.py:594-598`), so `_read_client_status` for it is 30. Spectators too.
- An item whose receiver is a group is delivered to **every member**, each in that member's own index space. The ItemSend print is one per check with `receiving` = the group id.
- Own-world filter quirk: the reference decides "from another world" by comparing the finder with the group id, so a member who found an item-link item in their own world still gets it without bit 1 (`MultiServer.py:1213-1215`). Read from code; intent not verified. Match it.
- Hints: `!hint` also searches items for groups containing you; hint prints and `_read_hints` lists go to every member; `UpdateHint` is allowed for any member.
- `Connected.players` in the reference lists every slot including groups and spectators (`MultiServer.py:859-860`, names from `slot_info`). Ours lists players only (`session.rs:306`). Include groups so clients can name the group in ItemSend prints.
- Connecting **as** a group slot: the reference accepts the name (group names are in `connect_names`, `Main.py:337` with `BaseClasses.py:202`) and then fails building `missing_locations` because the group has no location table (read from code, not run). Refuse with `InvalidSlot` (deviation, safer).
- State: room-core expands at apply. `Item` events gain `via: Option<SlotNo>` (the group), one event per member with that member's `item_seq`. Fold validates the expansion against `RoomOptions.groups`. Edge prints ItemSend from the first event of the expansion (or from the `Checked` event), not once per member.
- Importer and bundle: accept `SlotKind::Group` (`seed-bundle/src/lib.rs:245`, `ap_import.py:27-28, 88`).

### 4.3 Owners

`seed-bundle` and `tools/ap-import` (start inventory, Archipelago package, groups), `edge-compat` (index space, Handling bit 2, players list, prints), `room-core` (group expansion, group goal, group collect).

### 4.4 Scenarios

1. `a_client_with_the_start_inventory_flag_gets_start_items_first_from_index_zero`
2. `a_client_without_the_flag_never_sees_start_items_and_indexes_from_zero`
3. `live_items_continue_after_the_start_inventory_in_the_index`
4. `turning_on_the_start_inventory_flag_resends_everything_from_zero`
5. `the_archipelago_game_is_listed_and_its_package_is_served`
6. `an_item_for_a_group_reaches_every_member_with_their_own_index`
7. `a_group_item_found_by_a_member_reaches_that_member_without_the_own_items_flag`
8. `an_item_send_to_a_group_prints_once_naming_the_group`
9. `group_slots_read_as_goal_and_appear_in_the_players_list`
10. `connecting_as_a_group_slot_is_refused_with_invalid_slot`
11. `the_importer_keeps_start_inventory_groups_and_the_archipelago_package`

---

## 5. What a common client expects on connect that we do not send yet

Checked against the reference server and the reference client's connect path (`CommonClient.py:1036-1070`).

| Gap | Reference | Ours | Priority |
|---|---|---|---|
| `SetNotify` accepted | reference client sends Get + SetNotify for its hint key and both name-group keys on every connect | InvalidPacket "not supported" (`session.rs:237-241`) | high (section 1) |
| `_read_item_name_groups_{game}`, `_read_location_name_groups_{game}` | values from the data package (`MultiServer.py:609-612`) | `null`; the importer drops the groups; the reference client then caches `null` as the group table (`CommonClient.py:717-724`; effect on later use inferred) | high |
| ClientStatus CONNECTED (5) on first connection to a slot, back to UNKNOWN (0) when the slot's last connection leaves (goal is never undone) | `MultiServer.py:1045-1046, 1074-1075, 2315-2328` | only 0 and 30 | medium. Non-durable presence (a crash disconnects everyone, so 0 is correct after restart), held room-wide (room actor or `CompatRoom`), published to `_read_client_status` watchers. READY (10) and PLAYING (20) from `StatusUpdate` are kept the same way |
| `Tutorial` PrintJSON to the joining client | `MultiServer.py:1061-1065` | none | low; text only |
| `TagsChanged` PrintJSON on a ConnectUpdate that changes tags | `MultiServer.py:2057-2068` | none (compat-status lists it) | low |
| `RoomInfo.tags` | `["AP"]` by default (`MultiServer.py:387`) | `[]` | low; no client found that depends on it (**not verified** broadly) |
| `RoomInfo.games` and `DataPackage` include `Archipelago` | `MultiServer.py:559` | missing | high once start inventory lands (section 4.1); also used for the server's own item and location names |
| `Connected.players` includes groups and spectators | `MultiServer.py:859-860` | players only | medium (section 4.2) |
| Join print wording and `!help` mention | descriptive join line with version and tags (`MultiServer.py:1056-1060`) | short line | low; clients read the typed fields, not the words |
| "Team has completed all of their games" print when the last player of a team reaches goal | `MultiServer.py:2321-2324` | none | low |
| `RoomUpdate.checked_locations` full list after release/collect | sent as a second packet | will send only new ones (the doc allows partial) | low |

---

## 6. Where the protocol doc and the reference server disagree

All "server" sides are read from code at `MultiServer.py` / `NetUtils.py` 0.6.8, not run. When the server raises inside command handling, the exception escapes to the per-connection loop and the connection is closed (`MultiServer.py:994-1005`).

1. **Reserved prefix.** Doc "Set": key "can never start with `_read`". Server refuses only `_read_` (with the trailing underscore, `MultiServer.py:2286`), so a key like `_readme` is accepted.
2. **Operations that ignore `value`.** Doc "DataStorageOperation": `default`, `floor`, `ceil` ignore `value`. Server reads `value` for every operation (`MultiServer.py:2297`); an operation object without `value` closes the connection. We accept a missing `value` for those three.
3. **`Set` without `default`.** Doc says nothing; server starts from 0 (`MultiServer.py:2292`). Gap, not a conflict; we match 0.
4. **CreateHints, own unknown location.** Doc: "non-existing locations will silently be skipped". Server looks it up directly and the missing key closes the connection (`MultiServer.py:2138`; both lookup paths raise, `NetUtils.py` pure store and `_speedups.pyx:361-365`). We skip silently, as the doc says.
5. **CreateHints with status FOUND.** Doc: "Cannot set HINT_FOUND". Server accepts 40 (it only checks the value is a known status, `MultiServer.py:2117-2124`) and stores a hint with status FOUND while `found` is false. We refuse with InvalidPacket.
6. **UpdateHint `status` optional.** Doc marks it optional. Server reads it as required (`MultiServer.py:2162`); a missing field closes the connection (an explicit `null` is ignored). We treat missing like `null`.
7. **"Broadcast" hints from scouts.** Doc "LocationScouts": hints are "broadcasted". Server sends them only to the finder and receivers (`MultiServer.py:923-930`). We follow the server.
8. **Trap hints default to AVOID.** Doc "HintStatus" states it generally. Server applies it only to item hints (`!hint`); location hints (scouts, `!hint_location`, CreateHints) pass UNSPECIFIED explicitly, so a trap stays UNSPECIFIED (`MultiServer.py:1289-1296, 1306, 1316, 2100`). We follow the server.
9. **Release `auto`.** Doc "release": auto distributes on goal, says nothing about manual use. Server also allows manual `!release` after the goal in auto mode (`MultiServer.py:1605-1613`); `Permission` value 6 includes the goal bit. Same for collect.
10. **Permission `auto` scope.** Doc "Permission": auto works for release and collect. `NetUtils.py:59` comment says release only. Server code auto-collects (`MultiServer.py:949-950`), agreeing with the doc.
11. **`SetReply.slot`.** Doc lists `slot` without exception (only `original_value` is marked absent for `_read` keys). Server omits `slot` too for `_read_` notifications (`MultiServer.py:966, 972`).
12. **Unknown scout location.** Doc silent; server closes the connection (`MultiServer.py:2098`). Ours answers InvalidPacket (keep).
13. **`!` commands echoed as chat.** Not in the doc; server echoes every `!` text except `!admin` to all players (`MultiServer.py:1510-1514`).

---

## 7. Recommended build order

1. **Starting inventory + importer fields** (section 4.1, plus name groups, Archipelago package, start hints, entrances, spheres, blacklist into the bundle). Smallest change, no room-core events, unblocks most real seeds (start inventory is a common player option, **not verified** as a share).
2. **Cross-cutting plumbing + data storage** (sections 0 and 1). The reference client sends `SetNotify` on every connect, and hints and client status notifications ride on the same `_read_` / SetReply path.
3. **Client status presence** (section 5) on top of 2.
4. **Hints** (section 2): scouts with create_as_hint, CreateHints, UpdateHint, then `!hint` / `!hint_location` with the economy.
5. **Release, collect, remaining** (section 3), after the owner answers the policy question in 3.3.
6. **Group slots** (section 4.2) last: it touches item delivery, hints and collect.

## 8. Trickiest behaviors

1. Matching Python operator semantics for `Set` (floor-mod signs, tie-keeps-current in max/min, update on lists iterating dict keys or string characters, dict union with `or`, bool as int) with all-or-nothing and overflow caps.
2. SetReply delivery: pass-through fields and `original_value` are not state, so they ride as non-durable notes; the setter must get exactly one copy; a resyncing subscriber must still converge.
3. `!hint` economy and selection: one paid hint per command, sphere then non-local preference with a seeded tiebreak recorded in the event, found-at-creation hints shown but not stored, and `found` derived from checks so hint subscribers are notified when a check lands.
