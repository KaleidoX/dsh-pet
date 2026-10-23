use pet_model::{ThrowBounds, ThrowParams, ThrowSimulation, ThrowState};
use std::time::Duration;

fn run(frames: u32) -> ThrowState {
    let mut simulation = ThrowSimulation::new(ThrowState { x: 0.0, y: 0.0, vx: 150.0, vy: -200.0 });
    let bounds = ThrowBounds { min_x: -10000.0, max_x: 10000.0, min_y: -10000.0, max_y: 10000.0 };
    let params = ThrowParams { gravity: 100.0, restitution: 0.7, ground_friction: 0.8, ceiling_bounce: true, throw_power: 1.0 };
    for frame in 0..frames {
        let start = 2_000_000_000_u64 * u64::from(frame) / u64::from(frames);
        let end = 2_000_000_000_u64 * u64::from(frame + 1) / u64::from(frames);
        simulation.advance(Duration::from_nanos(end - start), bounds, params);
    }
    simulation.state()
}

#[test]
fn flight_speed_is_independent_of_redraw_rate() {
    let reference = run(120);
    for frames in [48, 60, 120, 240, 288] {
        let state = run(frames);
        assert_eq!((state.x, state.y, state.vx, state.vy), (reference.x, reference.y, reference.vx, reference.vy));
    }
    // Two seconds of ballistic motion: x=150*t, y=-200*t + 0.5*100*t².
    assert!((reference.x - 300.0).abs() < 0.1);
    assert!((reference.y + 200.0).abs() < 1.0);
    assert!(reference.vy.abs() < 0.01);
}

#[test]
fn irregular_redraws_do_not_reverse_a_free_flight() {
    let bounds = ThrowBounds { min_x: -10000.0, max_x: 10000.0, min_y: -10000.0, max_y: 10000.0 };
    let params = ThrowParams { gravity: 0.0, restitution: 0.7, ground_friction: 0.8, ceiling_bounce: true, throw_power: 1.0 };
    for velocity in [-2905.0, 2905.0] {
        let mut simulation = ThrowSimulation::new(ThrowState { x: 0.0, y: 0.0, vx: velocity, vy: 0.0 });
        let mut previous = 0.0;
        let mut elapsed = Duration::ZERO;
        for millis in [3, 4, 17, 2, 35, 1, 8, 16, 49, 0, 5, 33].into_iter().cycle().take(120) {
            let delta = Duration::from_millis(millis);
            elapsed += delta;
            let x = simulation.advance(delta, bounds, params).position.0;
            assert!((x - previous) * velocity >= 0.0, "position reversed: {previous} -> {x}");
            // Presentation may lag the constant-speed trajectory by one physics tick.
            let distance = velocity * elapsed.as_secs_f32();
            assert!((x - distance).abs() <= velocity.abs() / 120.0 + 0.1);
            previous = x;
        }
    }
}
