//! Divergence reporting used by `port-check`.

use data_fill::check::{Rules, SolveView, diff_solve, diff_spheres};
use serde_json::json;

fn v(xs: &[&[&str]]) -> Vec<Vec<String>> {
    xs.iter().map(|s| s.iter().map(|x| (*x).to_owned()).collect()).collect()
}

#[test]
fn spheres_diverge_at_the_first_round_that_differs() {
    let a = v(&[&["a", "b"], &["c"], &["d"]]);
    assert_eq!(diff_spheres(&a, &a), None);
    let b = v(&[&["b", "a"], &["c", "d"]]);
    assert_eq!(diff_spheres(&a, &b), Some((1, "d".into(), "data-only port only")));
    let short = v(&[&["a", "b"], &["c"]]);
    assert_eq!(diff_spheres(&a, &short), Some((2, "d".into(), "legacy only")));
}

#[test]
fn solve_diff_names_the_location_and_its_rules_before_regions() {
    let m = json!({"regions": [{"key": "r", "checks": [{"loc": "x", "rule": {"has": "k"}}]}]});
    let rules = Rules::new(&m);
    let legacy = SolveView::legacy(&json!({"locations": ["x"], "regions": {"r": 3}}));
    let ours = SolveView::legacy(&json!({"locations": [], "regions": {"r": 1}}));
    let d = diff_solve(&legacy, &ours, &rules).expect("differs");
    assert_eq!(d["location"], "x");
    assert_eq!(d["reached_by"], "legacy only");
    assert_eq!(d["rules"][0]["rule"], json!({"has": "k"}));
    let ours = SolveView::legacy(&json!({"locations": ["x"], "regions": {"r": 1}}));
    let d = diff_solve(&legacy, &ours, &rules).expect("mask differs");
    assert_eq!((d["kind"].as_str(), d["legacy_mask"].as_u64()), (Some("region"), Some(3)));
    assert_eq!(diff_solve(&legacy, &legacy, &rules), None);
}
