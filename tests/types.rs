use foil::{Metric, PeakMemory, Shrinkage, Time, Unit};

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
fn shrinkage_rejects_a_value_that_is_not_a_count() {
    for rejected in [f64::NAN, f64::INFINITY, -1.0] {
        assert!(Shrinkage::new(rejected).is_err(), "{rejected}");
    }

    assert_eq!(Shrinkage::new(0.0).ok(), Some(Shrinkage::NONE));
}
