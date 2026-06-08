use std::time::Duration;
use suprnova::queue::BackoffSchedule;
use suprnova::queue::retry::next_delay;

#[test]
fn fixed_backoff_returns_constant_delay() {
    let sched = BackoffSchedule::Fixed { secs: 5 };
    assert_eq!(next_delay(&sched, 1, Some(0.0)), Duration::from_secs(5));
    assert_eq!(next_delay(&sched, 7, Some(0.0)), Duration::from_secs(5));
}

#[test]
fn exponential_backoff_doubles_until_cap() {
    let sched = BackoffSchedule::Exponential {
        base_secs: 2,
        cap_secs: 60,
        jitter_ratio: 0.0,
    };
    assert_eq!(next_delay(&sched, 1, Some(0.0)), Duration::from_secs(2));
    assert_eq!(next_delay(&sched, 2, Some(0.0)), Duration::from_secs(4));
    assert_eq!(next_delay(&sched, 3, Some(0.0)), Duration::from_secs(8));
    assert_eq!(next_delay(&sched, 10, Some(0.0)), Duration::from_secs(60));
}

#[test]
fn exponential_jitter_stays_in_band() {
    let sched = BackoffSchedule::Exponential {
        base_secs: 10,
        cap_secs: 1000,
        jitter_ratio: 0.25,
    };
    // attempts=2 -> base_delay = 20. With jitter=±25%, range is [15, 25].
    // deterministic_jitter=Some(1.0) means max (+25%), Some(-1.0) means min (-25%).
    assert_eq!(next_delay(&sched, 2, Some(1.0)), Duration::from_secs(25));
    assert_eq!(next_delay(&sched, 2, Some(-1.0)), Duration::from_secs(15));
}

#[test]
fn exponential_jitter_at_one_does_not_exceed_cap() {
    // Pre-fix, jitter_ratio=1.0 with deterministic_jitter=+1.0
    // produced 2 × cap_secs because the post-jitter delay wasn't
    // re-capped. `cap_secs` is supposed to be a strict ceiling.
    let sched = BackoffSchedule::Exponential {
        base_secs: 10,
        cap_secs: 100,
        jitter_ratio: 1.0,
    };
    // attempts=10 → exponential schedule has saturated at cap (100s).
    // (1 + 1.0) * 100 = 200, but the final clamp pins it to 100.
    assert_eq!(next_delay(&sched, 10, Some(1.0)), Duration::from_secs(100));
}

#[test]
fn exponential_out_of_range_jitter_is_pinned_safely() {
    // jitter_ratio > 1.0 used to scale delays unbounded; NaN crashed
    // when round() was called on `nan`. Clamp + final cap pin both.
    let sched = BackoffSchedule::Exponential {
        base_secs: 10,
        cap_secs: 100,
        jitter_ratio: 5.0,
    };
    assert_eq!(next_delay(&sched, 10, Some(1.0)), Duration::from_secs(100));
    let nan_sched = BackoffSchedule::Exponential {
        base_secs: 10,
        cap_secs: 100,
        jitter_ratio: f32::NAN,
    };
    // NaN collapses to 0 — no jitter, plain capped delay.
    assert_eq!(
        next_delay(&nan_sched, 10, Some(1.0)),
        Duration::from_secs(100)
    );
}

#[test]
fn sequence_backoff_follows_explicit_steps() {
    let sched = BackoffSchedule::Sequence {
        secs: vec![1, 3, 9],
    };
    assert_eq!(next_delay(&sched, 1, Some(0.0)), Duration::from_secs(1));
    assert_eq!(next_delay(&sched, 2, Some(0.0)), Duration::from_secs(3));
    assert_eq!(next_delay(&sched, 3, Some(0.0)), Duration::from_secs(9));
    // beyond the sequence -> last entry sticks
    assert_eq!(next_delay(&sched, 99, Some(0.0)), Duration::from_secs(9));
}
