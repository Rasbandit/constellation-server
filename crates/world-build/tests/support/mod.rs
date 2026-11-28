//! The example world "Tidewell Isle", invented for these tests (no game's names, data or
//! logic), written as the model a front end would produce, and helpers to write models. Every
//! value gets the next line, so a test can find where any value "was written".

use std::cell::Cell;

use world_build::model::{
    At, Check, Def, Exit, Grant, Item, Lit, Members, Opt, Pos, Region, Row, Rule, RuleAt, Shuffle,
    Thing, Verb, World,
};

/// Hands out places, one line each.
#[derive(Default)]
pub struct F {
    line: Cell<usize>,
}

impl F {
    pub fn at<T>(&self, v: T) -> At<T> {
        let l = self.line.get() + 1;
        self.line.set(l);
        At::new(v, Pos::new(l, 1))
    }

    pub fn s(&self, v: &str) -> At<String> {
        self.at(v.to_owned())
    }

    pub fn list(&self, v: &[&str]) -> Vec<At<String>> {
        v.iter().map(|x| self.s(x)).collect()
    }

    /// A rule: `true` and `false` as constants, anything else as text.
    pub fn r(&self, text: &str) -> RuleAt {
        match text {
            "true" => self.at(Rule::Const(true)),
            "false" => self.at(Rule::Const(false)),
            t => self.at(Rule::Text(t.to_owned())),
        }
    }

    pub fn item(&self, key: &str, class: &str, count: Option<i64>) -> Item {
        Item {
            key: self.s(key),
            id: None,
            class: Some(self.s(class)),
            count: count.map(|c| self.at(c)),
            count_rule: None,
            later: vec![],
        }
    }

    pub fn check(&self, key: &str, rule: &str) -> Check {
        Check {
            also: false,
            key: self.s(key),
            id: None,
            rule: self.r(rule),
            when: None,
            vanilla: None,
            holds: None,
            no_progression: None,
            placement: None,
            later: vec![],
        }
    }

    pub fn exit(&self, to: &str, rule: &str) -> Exit {
        Exit { to: self.s(to), rule: self.r(rule), door: None, group: None }
    }

    pub fn door(&self, to: &str, rule: &str, door: &str, group: &str) -> Exit {
        Exit {
            to: self.s(to),
            rule: self.r(rule),
            door: Some(self.s(door)),
            group: Some(self.s(group)),
        }
    }

    pub fn region(
        &self,
        key: &str,
        checks: Vec<Check>,
        exits: Vec<Exit>,
        events: &[(&str, &str)],
    ) -> Region {
        Region {
            key: self.s(key),
            checks,
            exits,
            events: events.iter().map(|(e, r)| (self.s(e), self.r(r))).collect(),
            when: None,
            placement: None,
            can_stay: false,
            later: vec![],
        }
    }

    pub fn thing(&self, name: &str, rule: Option<&str>) -> Thing {
        Thing { name: self.s(name), rule: rule.map(|r| self.r(r)), group: None, later: vec![] }
    }

    pub fn names(&self, v: &[&str]) -> Members {
        Members::Names(v.iter().map(|n| self.thing(n, None)).collect())
    }

    pub fn row(&self, verb: Verb, subject: &[&str], object: &[&str]) -> Row {
        Row {
            verb: self.at(verb),
            subject: self.list(subject),
            object: self.list(object),
            later: vec![],
        }
    }
}

/// The smallest valid world of design doc 16 section 3.1 ("two rooms").
pub fn two_rooms() -> World {
    let f = F::default();
    World {
        format: Some(f.s("cgm-data/1")),
        game: Some(f.s("two_rooms")),
        start: Some(f.s("hall")),
        items: vec![f.item("key", "progression", None)],
        regions: vec![
            f.region(
                "hall",
                vec![f.check("shelf", "true")],
                vec![f.exit("vault", "has(`key`)")],
                &[],
            ),
            f.region("vault", vec![f.check("chest", "true")], vec![], &[]),
        ],
        goal: Some(f.r("reach(vault)")),
        ..World::default()
    }
}

/// Tidewell Isle: 9 regions, 17 checks, 4 options, a coupled entrance shuffle over three
/// doors from the dock, and a shuffle of two wardens whose rules the exits behind them read.
pub fn tidewell() -> World {
    let f = F::default();
    let opt = |key: &str, ty: &str, values: &[&str], range: Option<(i64, i64)>, default: Lit| Opt {
        key: f.s(key),
        ty: f.s(ty),
        values: f.list(values),
        min: range.map(|r| f.at(r.0)),
        max: range.map(|r| f.at(r.1)),
        default: Some(f.at(default)),
    };
    let options = vec![
        opt("tricky_climbs", "toggle", &[], None, Lit::Bool(false)),
        opt("pearls_needed", "range", &[], Some((1, 3)), Lit::Int(2)),
        opt("market_stalls", "toggle", &[], None, Lit::Bool(true)),
        opt("start_gift", "choice", &["none", "lamp"], None, Lit::Name("none".into())),
    ];
    let items = vec![
        f.item("oar", "progression", None),
        f.item("lamp", "progression", None),
        f.item("crowbar", "progression", None),
        f.item("pearl", "progression", Some(3)),
        f.item("chart", "useful", None),
        f.item("shell", "filler", None),
        f.item("gull", "trap", None),
    ];
    let stall = |key: &str| Check { when: Some(f.r("opt.market_stalls")), ..f.check(key, "true") };
    let regions = vec![
        f.region(
            "dock",
            vec![
                f.check("bollard", "true"),
                f.check("net_pile", "true"),
                Check { no_progression: Some(f.r("true")), ..f.check("tide_pool", "true") },
            ],
            vec![
                f.door("market", "true", "market_door", "outside"),
                f.door("old_mill", "true", "mill_door", "outside"),
                f.door("sea_cave", "has(oar)", "cave_mouth", "outside"),
                f.exit("lighthouse", "has(oar)"),
            ],
            &[],
        ),
        f.region(
            "market",
            vec![stall("fruit_stall"), stall("fish_stall"), f.check("notice_board", "true")],
            vec![f.door("dock", "true", "market_door", "inside")],
            &[],
        ),
        f.region(
            "old_mill",
            vec![f.check("millstone", "true"), f.check("flour_sacks", "has(crowbar)")],
            vec![
                f.door("dock", "true", "mill_door", "inside"),
                f.exit("mill_cellar", "has(crowbar)"),
            ],
            &[],
        ),
        f.region(
            "mill_cellar",
            vec![
                Check { holds: Some(f.s("chart")), ..f.check("old_chest", "true") },
                f.check("cobweb_corner", "true"),
            ],
            vec![f.exit("old_mill", "true")],
            &[],
        ),
        f.region(
            "sea_cave",
            vec![f.check("dripping_ledge", "true"), f.check("rock_pool", "has(lamp)")],
            vec![
                f.door("dock", "true", "cave_mouth", "inside"),
                f.exit("grotto", "at.wardens.grotto_gate"),
            ],
            &[],
        ),
        f.region(
            "grotto",
            vec![
                f.check("pearl_bed", "has(lamp)"),
                f.check("glowing_niche", "group_count(tools) >= 2"),
            ],
            vec![f.exit("sea_cave", "true")],
            &[],
        ),
        f.region(
            "lighthouse",
            vec![
                f.check("keeper_desk", "at.wardens.tower_door != crab or has(crowbar)"),
                f.check("spiral_stair", "can_climb"),
            ],
            vec![f.exit("dock", "true"), f.exit("lighthouse_top", "at.wardens.tower_door")],
            &[],
        ),
        f.region(
            "lighthouse_top",
            vec![f.check("lantern_room", "beacon_lit")],
            vec![f.exit("lighthouse", "true")],
            &[("beacon_lit", "has(lamp)")],
        ),
    ];
    let wardens = Shuffle {
        name: f.s("wardens"),
        slots: f.at(f.names(&["tower_door", "grotto_gate"])),
        things: Some(f.at(Members::Names(vec![
            f.thing("crab", Some("has(crowbar)")),
            f.thing("eel", Some("has(lamp)")),
        ]))),
        coupled: None,
        allow: vec![],
        check: None,
        later: vec![],
    };
    let doors = Shuffle {
        name: f.s("doors"),
        slots: f.at(Members::Exits(f.list(&["outside"]))),
        things: None,
        coupled: Some(f.r("true")),
        allow: vec![],
        check: Some(f.s("layout")),
        later: vec![],
    };
    World {
        format: Some(f.s("cgm-data/1")),
        game: Some(f.s("tidewell_isle")),
        version: Some(f.at(1)),
        name: Some(f.s("Tidewell Isle")),
        start: Some(f.s("dock")),
        options,
        items,
        fill_with: vec![(f.s("shell"), f.at(8)), (f.s("gull"), f.at(1))],
        start_items: vec![Grant {
            item: f.s("lamp"),
            n: None,
            when: Some(f.r("opt.start_gift == lamp")),
        }],
        defs: vec![Def::plain(f.s("can_climb"), f.r("has(crowbar) or opt.tricky_climbs"))],
        item_groups: vec![(f.s("tools"), f.list(&["oar", "lamp", "crowbar"]))],
        regions,
        goal: Some(f.r("beacon_lit and count(pearl) >= opt.pearls_needed")),
        shuffles: vec![doors, wardens],
        ..World::default()
    }
}
