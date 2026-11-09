//! Measure the library's embedded/cached thumbnail fast path without parsing meshes.
use std::{path::Path, time::Instant};
fn main() {
    let path = std::env::args().nth(1).expect("3MF path");
    let mut times = Vec::new();
    for _ in 0..101 {
        let start = Instant::now();
        let bytes = viewer::thumbnail_cached(Path::new(&path)).expect("embedded/cached thumbnail");
        std::hint::black_box(bytes);
        times.push(start.elapsed());
    }
    times.sort();
    println!(
        "thumbnail median {:.3}ms (101 reads)",
        times[50].as_secs_f64() * 1000.
    );
}
