use b3::{Metric, Pair, PeakMemory, Repetitions, RunOrder, Shrinkage, Side, Time, Unit};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};

#[test]
fn time_steps_down_through_si_prefixes() {
    for (seconds, scale, symbol) in [
        (12.0, 1.0, "s"),
        (1.0, 1.0, "s"),
        (0.999, 1e3, "ms"),
        (1e-3, 1e3, "ms"),
        (0.5e-3, 1e6, "µs"),
        (1e-6, 1e6, "µs"),
        (0.5e-6, 1e9, "ns"),
        (0.0, 1e9, "ns"),
    ] {
        let unit = Time::display_unit(Time::from_base(seconds));

        assert_eq!(unit, Unit { scale, symbol }, "{seconds}.");
    }
}

#[test]
fn memory_steps_up_through_decimal_prefixes() {
    for (bytes, scale, symbol) in [
        (3e9, 1e-9, "GB"),
        (1e9, 1e-9, "GB"),
        (5e8, 1e-6, "MB"),
        (1e6, 1e-6, "MB"),
        (2e3, 1e-3, "KB"),
        (1e3, 1e-3, "KB"),
        (999.0, 1.0, "B"),
        (0.0, 1.0, "B"),
    ] {
        let unit = PeakMemory::display_unit(PeakMemory::from_base(bytes));

        assert_eq!(unit, Unit { scale, symbol }, "{bytes}.");
    }
}

#[test]
fn a_fall_scales_like_a_rise() {
    let fall = Time::display_unit(Time::from_base(-2e-3));

    assert_eq!(fall, Time::display_unit(Time::from_base(2e-3)));
}

#[test]
fn sides_and_from_execution_order_agree() {
    let labelled = Pair {
        baseline: Side::Baseline,
        candidate: Side::Candidate,
    };

    for order in [RunOrder::BaselineFirst, RunOrder::CandidateFirst] {
        assert_eq!(
            Pair::from_execution_order(order.sides(), order),
            labelled,
            "{order:?}."
        );
    }
}

fn schedule(repetitions: usize, seed: u64) -> Vec<RunOrder> {
    RunOrder::schedule(repetitions, &mut Xoshiro256PlusPlus::seed_from_u64(seed))
}

fn baseline_firsts(orders: &[RunOrder]) -> usize {
    orders
        .iter()
        .filter(|&&order| order == RunOrder::BaselineFirst)
        .count()
}

#[test]
fn schedules_are_balanced() {
    for repetitions in [Repetitions::MINIMUM, Repetitions::MINIMUM + 1] {
        let orders = schedule(repetitions, 0);
        let first = baseline_firsts(&orders);

        assert_eq!(orders.len(), repetitions);
        assert!(
            first.abs_diff(repetitions - first) <= 1,
            "{repetitions} repetitions split {first}/{}.",
            repetitions - first
        );
    }
}

#[test]
fn the_odd_repetition_falls_on_either_side() {
    let counts: Vec<usize> = (0..16)
        .map(|seed| baseline_firsts(&schedule(Repetitions::MINIMUM + 1, seed)))
        .collect();

    assert!(counts.contains(&5) && counts.contains(&6), "{counts:?}");
}

#[test]
fn shrinkage_rejects_a_value_that_is_not_a_count() {
    for rejected in [f64::NAN, f64::INFINITY, -1.0] {
        assert!(Shrinkage::new(rejected).is_err(), "{rejected}");
    }

    assert_eq!(Shrinkage::new(0.0).ok(), Some(Shrinkage::NONE));
}
