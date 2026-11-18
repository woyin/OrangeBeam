//! Classroom countdown state machine (docs/COUNTDOWN.zh-CN.md, T1–T9).
use orange_beam::countdown::{
    clamp_minutes, Change, Countdown, FINISHED_SECONDS, MAX_MINUTES, MIN_MINUTES, SETUP_SECONDS,
};

const NOW: f64 = 100.0;

#[test]
fn toggle_starts_setup_with_the_last_used_minutes() {
    let mut countdown = Countdown::default();
    countdown.toggle(NOW, 7);
    let Countdown::Setup { minutes, start_at } = countdown else {
        panic!("idle must enter setup: {countdown:?}");
    };
    assert_eq!(minutes, 7);
    assert_eq!(start_at, NOW + SETUP_SECONDS);
    // A value outside the settable range is clamped, never rejected.
    countdown.cancel();
    countdown.toggle(NOW, 0);
    assert!(matches!(
        countdown,
        Countdown::Setup {
            minutes: MIN_MINUTES,
            ..
        }
    ));
}

#[test]
fn setup_starts_the_countdown_after_three_seconds() {
    let mut countdown = Countdown::default();
    countdown.toggle(NOW, 5);
    assert_eq!(countdown.tick(NOW + 2.9), None);
    assert_eq!(countdown.tick(NOW + 3.0), Some(Change::Started(5)));
    assert_eq!(countdown.remaining(NOW + 3.0), Some(300));
}

#[test]
fn every_adjustment_restarts_the_three_second_delay() {
    let mut countdown = Countdown::default();
    countdown.toggle(NOW, 5);
    countdown.adjust(1, NOW + 2.9);
    assert_eq!(
        countdown.tick(NOW + 3.0),
        None,
        "the original start time must be discarded"
    );
    assert_eq!(countdown.tick(NOW + 5.9), Some(Change::Started(6)));
}

#[test]
fn minutes_stop_at_one_and_ninety_nine() {
    let mut countdown = Countdown::default();
    countdown.toggle(NOW, 1);
    for step in 0..3 {
        countdown.adjust(-1, NOW + f64::from(step));
    }
    assert!(matches!(countdown, Countdown::Setup { minutes: 1, .. }));
    countdown.cancel();
    countdown.toggle(NOW, 99);
    for step in 0..3 {
        countdown.adjust(1, NOW + f64::from(step));
    }
    assert!(matches!(
        countdown,
        Countdown::Setup {
            minutes: MAX_MINUTES,
            ..
        }
    ));
    assert_eq!(clamp_minutes(0), MIN_MINUTES);
    assert_eq!(clamp_minutes(1_000), MAX_MINUTES);
}

#[test]
fn the_panel_stays_for_five_seconds_after_the_deadline() {
    let mut countdown = Countdown::default();
    countdown.toggle(NOW, 1);
    assert_eq!(countdown.tick(NOW + 3.0), Some(Change::Started(1)));
    let deadline = NOW + 3.0 + 60.0;
    assert_eq!(countdown.tick(deadline - 0.1), None);
    assert_eq!(countdown.tick(deadline), Some(Change::TimeUp));
    assert_eq!(countdown.tick(deadline + FINISHED_SECONDS - 0.1), None);
    assert_eq!(
        countdown.tick(deadline + FINISHED_SECONDS),
        Some(Change::Closed)
    );
    assert_eq!(countdown, Countdown::Idle);
}

#[test]
fn toggle_returns_to_idle_from_every_non_idle_state() {
    let mut countdown = Countdown::default();
    for stop in 0..3 {
        countdown.toggle(NOW, 5);
        // Setup, running and the "time is up" panel.
        let now = [NOW + 1.0, NOW + 3.5, NOW + 70.0][stop];
        let _ = countdown.tick(now);
        countdown.toggle(now, 5);
        assert_eq!(countdown, Countdown::Idle, "state after tick {stop}");
    }
}

#[test]
fn adjusting_outside_setup_changes_nothing() {
    let mut countdown = Countdown::default();
    countdown.adjust(1, NOW);
    assert_eq!(countdown, Countdown::Idle);
    countdown.toggle(NOW, 5);
    countdown.tick(NOW + 3.0);
    let running = countdown;
    countdown.adjust(1, NOW + 4.0);
    assert_eq!(countdown, running);
    let mut finished = Countdown::Running { deadline: NOW };
    finished.adjust(-1, NOW);
    assert_eq!(finished, Countdown::Running { deadline: NOW });
}

#[test]
fn only_setup_asks_for_the_page_keys() {
    let mut countdown = Countdown::default();
    assert!(!countdown.wants_page_keys());
    countdown.toggle(NOW, 5);
    assert!(countdown.wants_page_keys());
    countdown.tick(NOW + 3.0);
    assert!(!countdown.wants_page_keys());
    let finished = Countdown::Finished { until: NOW + 1.0 };
    assert!(!finished.wants_page_keys());
    countdown.cancel();
    assert!(!countdown.wants_page_keys());
}

#[test]
fn remaining_rounds_up_so_the_last_second_still_shows_one() {
    let countdown = Countdown::Running {
        deadline: NOW + 60.0,
    };
    assert_eq!(countdown.remaining(NOW), Some(60));
    assert_eq!(countdown.remaining(NOW + 59.8), Some(1));
    assert_eq!(countdown.remaining(NOW + 60.0), Some(0));
    assert_eq!(countdown.remaining(NOW + 61.0), Some(0));
    assert_eq!(Countdown::Idle.remaining(NOW), None);
    let setting_up = Countdown::Setup {
        minutes: 5,
        start_at: NOW,
    };
    assert_eq!(
        setting_up.remaining(NOW),
        None,
        "setup shows minutes, not seconds"
    );
}
