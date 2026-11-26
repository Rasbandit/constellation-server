//! The pool's two sides without a process: the worker's loop over scripted input, the server's
//! side against scripted answers, and both together over a socket pair. The same through real
//! worker processes is `tests/pool.rs`.

use super::*;
use data_fill::Instance;
use data_fill::room::{Algorithm, Dials};
use data_fill::synth::Shape;
use fills::{alone, head, prepared, small};

/// Shared with `tests/pool.rs`: the worlds, the requests and the one-process answer.
#[path = "../../tests/support/fills.rs"]
mod fills;

fn message(kind: Kind, body: &[u8]) -> Vec<u8> {
    [&header(kind, body.len()).unwrap()[..], body].concat()
}

fn job_message(head: &[u8], key: &[u8]) -> Vec<u8> {
    message(Kind::Job, &job::frame(head, key).unwrap())
}

/// A worker over the scripted `input`, keeping its worlds in `worlds`: how it ended and
/// every message it answered.
fn scripted(
    input: &[u8],
    worlds: &mut Cache<Loaded>,
) -> (Result<(), String>, Vec<(Kind, Vec<u8>)>) {
    let mut out = Vec::new();
    let ended = serve_keeping(input, &mut out, worlds);
    let (mut answers, mut rest) = (Vec::new(), &out[..]);
    while let Some(m) = read(&mut rest).unwrap() {
        answers.push(m);
    }
    (ended, answers)
}

fn roomy() -> Cache<Loaded> {
    Cache::new(config::PREPARED_CACHE_BYTES)
}

fn kept(worlds: &Cache<Loaded>) -> Vec<BlobKey> {
    let mut keys: Vec<BlobKey> = worlds.held.keys().copied().collect();
    keys.sort_unstable();
    keys
}

/// Byte identity: one worker takes every request in turn, on two worlds, for one and ten
/// players and each fill mode, and answers each with the bytes a job child of its own prints
/// (or the same refusal), with the world sent the first time (cold) and from its cache after
/// (warm). Fails if a fill depended on anything a job before it left.
#[test]
fn a_worker_answers_job_after_job_with_the_bytes_of_a_job_in_its_own_process() {
    let modes = [
        Algorithm::Exact,
        Algorithm::Level(Dials::default()),
        Algorithm::Shuffle(Dials::default()),
    ];
    let worlds = [prepared(&small(), 1), prepared(&Shape { checks: 200, ..Shape::heavy() }, 2)];
    let ten: Vec<String> = (0..10).map(|p| format!("P{p}")).collect();
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    let (mut script, mut expected) = (Vec::new(), Vec::new());
    let mut seen = Vec::new();
    for rng in 1..=2 {
        for mode in modes {
            for names in [&["Ann"][..], &ten] {
                for (key, file, instance) in &worlds {
                    let head = head(names, instance, rng, mode);
                    script.extend(job_message(&head, key));
                    if !seen.contains(key) {
                        seen.push(*key);
                        script.extend(message(Kind::World, file));
                        expected.push((Kind::Miss, Vec::new()));
                    }
                    expected.push(match alone(&head, file) {
                        Ok(seed) => (Kind::Done, seed),
                        Err(why) => (Kind::Refused, why.into_bytes()),
                    });
                }
            }
        }
    }
    let (ended, answers) = scripted(&script, &mut roomy());
    assert_eq!(ended, Ok(()));
    let done = answers.iter().filter(|(kind, _)| *kind == Kind::Done).count();
    assert!(done >= 20, "only {done} of 24 jobs filled: too few to show identity");
    assert_eq!(answers.len(), expected.len());
    for (n, (got, want)) in answers.iter().zip(&expected).enumerate() {
        assert!(got == want, "answer {n} differs from the job in its own process");
    }
}

/// What a worker holds after its jobs is its loaded worlds and nothing of a request: a job on
/// another world, and a later job on the first one, get exactly the bytes they get alone, and
/// no name of an earlier job's players is in them.
#[test]
fn a_job_gets_nothing_of_the_jobs_before_it() {
    let (x, y) = (prepared(&small(), 1), prepared(&small(), 2));
    let first = head(&["Ann-of-job-one", "Bo-of-job-one"], &x.2, 5, Algorithm::Exact);
    let other = head(&["Cy"], &y.2, 5, Algorithm::Exact);
    let again = head(&["Dee"], &x.2, 9, Algorithm::Exact);
    let script = [
        job_message(&first, &x.0),
        message(Kind::World, &x.1),
        job_message(&other, &y.0),
        message(Kind::World, &y.1),
        job_message(&again, &x.0),
    ]
    .concat();
    let mut worlds = roomy();
    let (ended, answers) = scripted(&script, &mut worlds);
    assert_eq!(ended, Ok(()));
    let kinds: Vec<Kind> = answers.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(kinds, [Kind::Miss, Kind::Done, Kind::Miss, Kind::Done, Kind::Done]);
    let has = |seed: &[u8], name: &str| seed.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has(&answers[1].1, "Ann-of-job-one"), "a seed carries its own players' names");
    for (answer, head, file) in [(&answers[3].1, &other, &y.1), (&answers[4].1, &again, &x.1)] {
        assert_eq!(answer, &alone(head, file).unwrap());
        assert!(!has(answer, "of-job-one"), "nothing of the first job's players");
    }
    // All that is left in the worker: the two worlds, under the hashes of their files.
    let mut both = [x.0, y.0];
    both.sort_unstable();
    assert_eq!(kept(&worlds), both);
    assert_eq!(worlds.used, x.1.len() + y.1.len());
}

/// A file is kept only under the key it hashes to: sent under another world's key it ends the
/// worker, before it is loaded, so no later job naming that key can be filled on it.
#[test]
fn a_world_that_does_not_hash_to_its_key_is_never_loaded_or_kept() {
    let (x, y) = (prepared(&small(), 1), prepared(&small(), 2));
    let head = head(&["Ann"], &x.2, 1, Algorithm::Exact);
    let script = [job_message(&head, &x.0), message(Kind::World, &y.1)].concat();
    let mut worlds = roomy();
    let (ended, answers) = scripted(&script, &mut worlds);
    assert!(ended.unwrap_err().contains("do not hash to its key"));
    assert_eq!(answers, [(Kind::Miss, Vec::new())], "it asked for the world and answered nothing");
    assert!(worlds.held.is_empty());
}

/// A file that hashes to its key and still does not load is the job's refusal, worded as the
/// one-process job words it (the server prepares the world again on that); it is not kept, and
/// the worker itself is sound.
#[test]
fn a_world_that_does_not_load_is_refused_as_the_job_child_refuses_it_and_not_kept() {
    let x = prepared(&small(), 1);
    let junk = b"not a prepared world".to_vec();
    let head = head(&["Ann"], &x.2, 1, Algorithm::Exact);
    let script = [
        job_message(&head, &log::blob_key(&junk)),
        message(Kind::World, &junk),
        job_message(&head, &x.0),
        message(Kind::World, &x.1),
    ]
    .concat();
    let mut worlds = roomy();
    let (ended, answers) = scripted(&script, &mut worlds);
    assert_eq!(ended, Ok(()));
    assert_eq!(answers[1], (Kind::Refused, alone(&head, &junk).unwrap_err().into_bytes()));
    assert!(String::from_utf8_lossy(&answers[1].1).starts_with(job::UNLOADABLE));
    assert_eq!(answers[3], (Kind::Done, alone(&head, &x.1).unwrap()));
    assert_eq!(kept(&worlds), [x.0]);
    // A request that is refused (here not JSON) leaves the world kept and the worker going.
    let script = [job_message(b"{", &x.0), job_message(&head, &x.0)].concat();
    let (ended, answers) = scripted(&script, &mut worlds);
    assert_eq!(ended, Ok(()));
    assert_eq!(answers[0], (Kind::Refused, alone(b"{", &x.1).unwrap_err().into_bytes()));
    assert_eq!(answers[1].0, Kind::Done);
}

/// The kept worlds stay under the cap, the least recently used going first; a cap of nothing
/// keeps nothing and every job still fills.
#[test]
fn kept_worlds_stay_under_the_cap_and_the_least_recently_used_goes_first() {
    let (x, y) = (prepared(&small(), 1), prepared(&small(), 2));
    let (hx, hy) =
        (head(&["Ann"], &x.2, 1, Algorithm::Exact), head(&["Bo"], &y.2, 1, Algorithm::Exact));
    let cold = |head: &[u8], w: &(BlobKey, Vec<u8>, Instance)| {
        [job_message(head, &w.0), message(Kind::World, &w.1)].concat()
    };
    let script = [cold(&hx, &x), cold(&hy, &y), cold(&hx, &x)].concat();
    let miss_done = [Kind::Miss, Kind::Done];
    for (cap, left) in [(x.1.len() + y.1.len() - 1, vec![x.0]), (0, vec![])] {
        let mut worlds = Cache::new(cap);
        let (ended, answers) = scripted(&script, &mut worlds);
        assert_eq!(ended, Ok(()), "cap {cap}");
        let kinds: Vec<Kind> = answers.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, [miss_done, miss_done, miss_done].concat(), "cap {cap}: each a miss");
        assert_eq!(answers[5].1, alone(&hx, &x.1).unwrap());
        assert_eq!(kept(&worlds), left, "cap {cap}");
        assert!(worlds.used <= cap);
    }
}

/// Hostile or damaged input ends the worker without an answer: an unknown kind, a message
/// out of turn, a header or body cut short, a key of the wrong length, a job where the asked
/// world should be. No input at all is a clean end.
#[test]
fn a_worker_stops_on_anything_but_a_whole_message_in_turn() {
    let x = prepared(&small(), 1);
    let job = job_message(&head(&["Ann"], &x.2, 1, Algorithm::Exact), &x.0);
    let world = message(Kind::World, &x.1);
    let short_key = job_message(b"{}", &[7; 31]);
    let bad: [(&str, Vec<u8>); 9] = [
        ("an unknown kind", vec![9, 0, 0, 0, 0]),
        ("a world before any job", world.clone()),
        ("an answer kind", message(Kind::Done, b"seed")),
        ("a header cut short", job[..3].to_vec()),
        ("a body cut short", job[..job.len() - 1].to_vec()),
        ("a length that lies", [&header(Kind::Job, 1 << 30).unwrap()[..], b"x"].concat()),
        ("a key of 31 bytes", short_key),
        ("no world after a miss", job.clone()),
        ("a job in place of the world", [&job[..], &job[..]].concat()),
    ];
    for (what, input) in bad {
        let mut worlds = roomy();
        let (ended, answers) = scripted(&input, &mut worlds);
        assert!(ended.is_err(), "{what}");
        assert!(answers.iter().all(|(kind, _)| *kind == Kind::Miss), "{what}: {answers:?}");
        assert!(worlds.held.is_empty(), "{what}");
    }
    assert_eq!(scripted(&[], &mut roomy()), (Ok(()), vec![]));
    // The whole job after all that: a worker's refusals leave nothing behind.
    let (ended, answers) = scripted(&[job, world].concat(), &mut roomy());
    assert_eq!((ended, answers.len()), (Ok(()), 2));
}

/// The server's side against scripted answers `said`: what [`ask`] makes of them.
async fn told(said: &[u8], l: &Limits) -> Result<Result<Vec<u8>, String>, String> {
    use tokio::io::AsyncWriteExt as _;
    let (ours, mut theirs) = tokio::io::duplex(1 << 20);
    theirs.write_all(said).await.unwrap();
    theirs.shutdown().await.unwrap();
    let (mut from, mut to) = tokio::io::split(ours);
    let asked = ask((&mut to, &mut from), b"{}", (&[0; 32], b"world"), l).await;
    drop(theirs);
    asked
}

/// The server trusts no answer: a seed or a refusal is taken (a refusal cut to the end the
/// error cap allows), a second miss, a kind out of turn or unknown, an answer over the output
/// cap or cut short all break the worker, and nothing of an oversized answer is read.
#[tokio::test]
async fn the_server_side_takes_a_seed_or_a_refusal_and_nothing_else() {
    let l = Limits { gen_stdout_max_bytes: 8, gen_error_max_bytes: 4, ..Limits::DESIGN };
    let miss = message(Kind::Miss, &[]);
    assert_eq!(told(&message(Kind::Done, b"12345678"), &l).await, Ok(Ok(b"12345678".to_vec())));
    let after_miss = [&miss[..], &message(Kind::Done, b"seed")].concat();
    assert_eq!(told(&after_miss, &l).await, Ok(Ok(b"seed".to_vec())));
    assert_eq!(told(&message(Kind::Refused, b"too long"), &l).await, Ok(Err("long".into())));
    let broken = [
        ("more than 8 bytes", message(Kind::Done, b"123456789")),
        ("more than 8 bytes", header(Kind::Done, u32::MAX as usize).unwrap().to_vec()),
        ("Miss out of turn", [&miss[..], &miss[..]].concat()),
        ("World out of turn", message(Kind::World, b"w")),
        ("Job out of turn", message(Kind::Job, b"j")),
        ("unknown pool message kind 0", vec![0, 0, 0, 0, 0]),
        ("cut short", message(Kind::Done, b"1234")[..7].to_vec()),
        ("its pipe", vec![Kind::Done as u8, 1]),
        ("its pipe", vec![]),
    ];
    for (why, said) in broken {
        let e = told(&said, &l).await.unwrap_err();
        assert!(e.contains(why), "{why}: {e}");
    }
}

/// Both sides over a socket pair, the worker's loop on a thread: the first fill sends the
/// world, the second finds it kept (the file is not sent again: junk in its place changes
/// nothing), and both are the bytes of the job in its own process.
#[tokio::test]
async fn the_world_is_sent_on_a_miss_only_and_the_seed_is_the_same_cold_and_warm() {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let reader = theirs.try_clone().unwrap();
    let worker = std::thread::spawn(move || serve(reader, theirs, config::PREPARED_CACHE_BYTES));
    ours.set_nonblocking(true).unwrap();
    let (mut from, mut to) = tokio::net::UnixStream::from_std(ours).unwrap().into_split();
    let (key, file, instance) = prepared(&small(), 1);
    let head = head(&["Ann"], &instance, 3, Algorithm::Exact);
    let want = alone(&head, &file).unwrap();
    let l = Limits::DESIGN;
    let cold = ask((&mut to, &mut from), &head, (&key, &file), &l).await;
    assert_eq!(cold, Ok(Ok(want.clone())));
    let warm = ask((&mut to, &mut from), &head, (&key, b"never sent"), &l).await;
    assert_eq!(warm, Ok(Ok(want)));
    // The server closing its end lets the worker go.
    drop((from, to));
    assert_eq!(worker.join().unwrap(), Ok(()));
}

/// A worker's CPU cap counts its whole life, so it is replaced while a whole job still fits:
/// with the design limits (120 CPU seconds, jobs of at most 60 s on one thread) that is up to
/// 60 s of jobs. Where no whole job fits even in a fresh worker, none is kept for a second.
#[test]
fn a_worker_is_replaced_before_its_cpu_cap_could_end_a_job_that_did_nothing_wrong() {
    let l = Limits::DESIGN;
    assert_eq!((l.fill_cpu_secs, l.fill_timeout_ms, config::GENERATION_WORKERS), (120, 60_000, 1));
    assert!(has_cpu_left(Duration::ZERO, &l));
    assert!(has_cpu_left(Duration::from_millis(59_999), &l));
    assert!(!has_cpu_left(Duration::from_secs(60), &l));
    assert!(!has_cpu_left(Duration::ZERO, &Limits { fill_cpu_secs: 60, ..l }));
    assert!(!has_cpu_left(Duration::MAX, &l));
}

/// A worker is started as a job child is: the same program under the same caps, in job mode.
#[test]
fn a_worker_runs_under_the_caps_of_a_job_child() {
    let l = Limits { fill_memory_bytes: 7, fill_cpu_secs: 3, ..Limits::DESIGN };
    let (worker, child) = (
        job_argv(std::path::Path::new("/x/server"), WORKER, &l),
        job_argv(std::path::Path::new("/x/server"), "fill", &l),
    );
    assert_eq!(worker.last().unwrap(), "worker");
    assert_eq!(worker[..worker.len() - 1], child[..child.len() - 1]);
}
