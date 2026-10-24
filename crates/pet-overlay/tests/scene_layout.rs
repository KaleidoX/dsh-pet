use pet_overlay::SceneLayout;

#[test]
fn compact_motion_keeps_the_image_at_the_window_origin() {
    let world = pet_config::WorldArea { x: 0, y: 0, width: 3420, height: 2224 };
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut layout = SceneLayout::compact(scale);
        assert_eq!(layout.size, (640, 360));
        for x in -200..3600 {
            let canvas = (x, x / 2);
            assert_eq!(layout.place(canvas, world), canvas);
            assert_eq!(layout.offset, (0, 0));
        }
    }
}

#[test]
fn expanding_and_collapsing_controls_preserves_screen_position() {
    let world = pet_config::WorldArea { x: 120, y: 86, width: 3300, height: 2138 };
    for canvas in [(-80, 86), (2732, 286), (3200, 1864), (1200, 800)] {
        let mut compact = SceneLayout::compact(2.0);
        let mut controls = SceneLayout::new(2.0);
        let window = controls.place(canvas, world);
        assert_eq!(controls.canvas_origin(window), canvas);
        let collapsed = compact.place(controls.canvas_origin(window), world);
        assert_eq!(collapsed, canvas);
    }
}

#[test]
fn interaction_region_is_derived_from_the_video_screen_position() {
    let world = pet_config::WorldArea { x: 120, y: 86, width: 3300, height: 2138 };
    for scale in [1.0, 1.25, 1.5, 2.0] {
        for canvas in [(-80, 86), (2732, 286), (3200, 1864), (1200, 800)] {
            for mut layout in [SceneLayout::compact(scale), SceneLayout::new(scale)] {
                let window = layout.place(canvas, world);
                let hit = layout.hit_rect();
                let min = (hit.min.x * scale + window.0 as f32, hit.min.y * scale + window.1 as f32);
                let max = (hit.max.x * scale + window.0 as f32, hit.max.y * scale + window.1 as f32);
                assert!((min.0 - (canvas.0 + 200) as f32).abs() < 0.01);
                assert!((min.1 - (canvas.1 + 50) as f32).abs() < 0.01);
                assert!((max.0 - (canvas.0 + 440) as f32).abs() < 0.01);
                assert!((max.1 - (canvas.1 + 335) as f32).abs() < 0.01);
            }
        }
    }
}

#[test]
fn screen_edge_padding_does_not_displace_the_canvas() {
    let world = pet_config::WorldArea { x: 2160, y: 840, width: 3840, height: 2160 };
    let mut layout = SceneLayout::new(1.5);
    let canvas = (5324, 990);
    let window = layout.place(canvas, world);
    assert_eq!(window, (4860, 840));
    assert_eq!(layout.canvas_origin(window), canvas);
    assert_eq!(layout.offset, (464, 150));
}

#[test]
fn controls_keep_logical_size_and_pet_keeps_physical_size() {
    for (scale, size, offset) in
        [(0.5, (640, 360), (0, 0)), (1.0, (760, 480), (60, 60)), (1.5, (1140, 720), (250, 180)), (2.0, (1520, 960), (440, 300))]
    {
        let layout = SceneLayout::new(scale);
        assert_eq!(layout.size, size);
        assert_eq!(layout.offset, offset);
        assert_eq!(layout.size.0 - 2 * offset.0 as u32, 640);
        assert_eq!(layout.size.1 - 2 * offset.1 as u32, 360);
    }
}

#[test]
fn padding_does_not_change_home_or_drag_coordinates() {
    for scale in [0.5, 1.0, 1.25, 1.5, 1.75, 2.0] {
        let layout = SceneLayout::new(scale);
        for canvas in [(-1920, -1080), (616, 100), (3840, 2160)] {
            assert_eq!(layout.canvas_origin(layout.window_origin(canvas)), canvas);
        }
    }
}

#[test]
fn input_region_tracks_the_drawn_pet_at_fractional_scale() {
    let layout = SceneLayout::new(1.5);
    let hit = layout.hit_rect();
    // The established canvas hit box is x=200..440, y=50..335 physical pixels.
    assert_eq!((hit.min.x * 1.5).round(), 450.0);
    assert_eq!((hit.min.y * 1.5).round(), 230.0);
    assert_eq!((hit.max.x * 1.5).round(), 690.0);
    assert_eq!((hit.max.y * 1.5).round(), 515.0);
}
