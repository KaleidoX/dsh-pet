//! Verify the public native decoder API against real MOV clips (requires macOS).
#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    for path in std::env::args_os().skip(1) {
        let path = std::path::Path::new(&path);
        let start = std::time::Instant::now();
        let mut decoder = pet_macos::Decoder::open(path)?;
        let mut frames = 0;
        let mut previous = f64::NEG_INFINITY;
        let mut size = None;
        while let Some((frame, pts)) = decoder.next_frame()? {
            anyhow::ensure!(pts.is_finite() && pts >= previous, "invalid PTS in {}", path.display());
            let dimensions = (frame.width, frame.height);
            anyhow::ensure!(size.is_none_or(|size| size == dimensions), "dimensions changed");
            size = Some(dimensions);
            previous = pts;
            frames += 1;
        }
        anyhow::ensure!(frames > 0, "empty clip");
        println!(
            "{} hardware=true frames={frames} size={size:?} duration={:.6} last_pts={previous:.6} decode_ms={:.1}",
            path.display(),
            decoder.duration,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This probe requires macOS.");
    std::process::exit(1);
}
