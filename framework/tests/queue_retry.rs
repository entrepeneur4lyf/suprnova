use std::time::Duration;
use suprnova::queue::retry::next_delay;
use suprnova::queue::BackoffSchedule;

#[test]
fn fixed_backoff_returns_constant_delay() {
    let sched = BackoffSchedule::Fixed { secs: 5 };
    assert_eq!(next_delay(&sched, 1, Some(0.0)), Duration::from_secs(5));
    assert_eq!(next_delay(&sched, 7, Some(0.0)), Duration::from_secs(5));
}

#[test]
fn exponential_backoff_doubles_until_cap() {
    let sched = BackoffSchedule::Exponential { base_secs: 2, cap_secs: 60, jitter_ratio: 0.0 };
    assert_eq!(next_delay(&sched, 1, Some(0.0)), Duration::from_secs(2));
    assert_eq!(next_delay(&sched, 2, Some(0.0)), Duration::from_secs(4));
    assert_eq!(next_delay(&sched, 3, Some(0.0)), Duration::from_secs(8));
    assert_eq!(next_delay(&sched, 10, Some(0.0)), Duration::from_secs(60));
}

#[test]
fn exponential_jitter_stays_in_band() {
    let sched = BackoffSchedule::Exponential { base_secs: 10, cap_secs: 1000, jitter_ratio: 0.25 };
    // attempts=2 -> base_delay = 20. With jitter=±25%, range is [15, 25].
    // deterministic_jitter=Some(1.0) means max (+25%), Some(-1.0) means min (-25%).
    assert_eq!(next_delay(&sched, 2, Some(1.0)), Duration::from_secs(25));
    assert_eq!(next_delay(&sched, 2, Some(-1.0)), Duration::from_secs(15));
}

#[test]
fn sequence_backoff_follows_explicit_steps() {
    let sched = BackoffSchedule::Sequence { secs: vec![1, 3, 9] };
    assert_eq!(next_delay(&sched, 1, Some(0.0)), Duration::from_secs(1));
    assert_eq!(next_delay(&sched, 2, Some(0.0)), Duration::from_secs(3));
    assert_eq!(next_delay(&sched, 3, Some(0.0)), Duration::from_secs(9));
    // beyond the sequence -> last entry sticks
    assert_eq!(next_delay(&sched, 99, Some(0.0)), Duration::from_secs(9));
}
