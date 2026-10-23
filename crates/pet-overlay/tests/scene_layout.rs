use pet_overlay::SceneLayout;

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
