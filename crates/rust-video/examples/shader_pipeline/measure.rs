//! Median-of-samples timing with a spread gate.

/// Timed pass over one clip.
pub struct Sample {
    /// Decoded frames in the pass.
    pub frames: usize,
    /// Frames per second.
    pub fps: f64,
}

/// Median frames per second over `samples`.
///
/// # Panics
///
/// Panics when `samples` is empty.
#[must_use]
pub fn median(samples: &[Sample]) -> f64 {
    let mut fps: Vec<f64> = samples.iter().map(|sample| sample.fps).collect();
    fps.sort_by(f64::total_cmp);
    let mid = fps.len() / 2;
    if fps.len().is_multiple_of(2) {
        fps[mid - 1].midpoint(fps[mid])
    } else {
        fps[mid]
    }
}

/// Relative spread of `samples` around their median: `(max - min) / median`.
#[must_use]
pub fn spread(samples: &[Sample], median_fps: f64) -> f64 {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for sample in samples {
        min = min.min(sample.fps);
        max = max.max(sample.fps);
    }
    if median_fps > 0.0 {
        (max - min) / median_fps
    } else {
        0.0
    }
}

/// Five timed passes with up to three outlier rerun rounds.
///
/// Samples outside +-10% of the median rerun; the reported median and
/// spread cover the final five.
pub fn median_of_5(mut run: impl FnMut() -> Sample) -> (f64, f64, usize, bool) {
    let mut samples: Vec<Sample> = (0..5).map(|_| run()).collect();
    let mut reran = false;
    for _ in 0..3 {
        let center = median(&samples);
        if spread(&samples, center) <= 0.10 || center <= 0.0 {
            break;
        }
        for sample in &mut samples {
            if (sample.fps - center).abs() / center > 0.10 {
                *sample = run();
                reran = true;
            }
        }
    }
    let fps = median(&samples);
    let spread = spread(&samples, fps);
    let frames = samples.iter().map(|sample| sample.frames).sum::<usize>() / samples.len();
    (fps, spread, frames, reran)
}
