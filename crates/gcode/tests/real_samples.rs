use std::path::PathBuf;
use std::time::Instant;

use gcode::parse_file;

// Real model exports are optional local fixtures, never implicitly read from HOME.
// Set SLICENV_GCODE_SAMPLES to a directory containing the two export subdirectories.
fn samples_root() -> Option<PathBuf> {
    std::env::var_os("SLICENV_GCODE_SAMPLES").map(PathBuf::from)
}

fn sample_shelves(root: &std::path::Path) -> PathBuf {
    let p = root.join("All+Shelves-plate2/plate_2.gcode");
    if p.exists() {
        p
    } else {
        root.join("All+Shelves-plate2/plate_1.gcode")
    }
}

#[test]
fn test_all_shelves_real_sample() {
    let Some(root) = samples_root() else {
        eprintln!("Set SLICENV_GCODE_SAMPLES to test real All+Shelves exports");
        return;
    };
    let path = sample_shelves(&root);
    assert!(
        path.exists(),
        "sample file does not exist: {}",
        path.display()
    );

    let started = Instant::now();
    let toolpath = parse_file(&path).expect("failed to parse All+Shelves G-code");
    let elapsed = started.elapsed();

    println!(
        "Parsed {} in {:.1} ms: {} layers, {} segments",
        path.display(),
        elapsed.as_secs_f64() * 1000.0,
        toolpath.layers.len(),
        toolpath.segments.len()
    );

    // Перевірка продуктивності (< 300 мс на 5 МБ файлі)
    assert!(
        elapsed.as_millis() < 300,
        "Parsing took too long: {} ms (target < 300 ms)",
        elapsed.as_millis()
    );

    // 1. Кількість шарів збігається із заголовком
    assert_eq!(
        toolpath.stats.total_layer_number,
        Some(250),
        "total_layer_number in header should be 250"
    );
    assert_eq!(
        toolpath.layers.len(),
        250,
        "parsed layer count must match header"
    );

    // 2. Адекватні значення Z
    let mut prev_z = -1.0f32;
    for (i, layer) in toolpath.layers.iter().enumerate() {
        assert!(!layer.z.is_nan(), "layer {} z is NaN", i);
        assert!(!layer.height.is_nan(), "layer {} height is NaN", i);
        assert!(layer.z > 0.0, "layer {} z is not positive: {}", i, layer.z);
        assert!(
            layer.z >= prev_z,
            "layer {} z decreased from {} to {}",
            i,
            prev_z,
            layer.z
        );
        prev_z = layer.z;
    }

    // 3. Відсутність NaN у всіх сегментах
    for (i, seg) in toolpath.segments.iter().enumerate() {
        assert!(
            !seg.from.x.is_nan() && !seg.from.y.is_nan() && !seg.from.z.is_nan(),
            "seg {} from is NaN",
            i
        );
        assert!(
            !seg.to.x.is_nan() && !seg.to.y.is_nan() && !seg.to.z.is_nan(),
            "seg {} to is NaN",
            i
        );
        assert!(!seg.width.is_nan(), "seg {} width is NaN", i);
        assert!(!seg.height.is_nan(), "seg {} height is NaN", i);
        assert!(!seg.feedrate.is_nan(), "seg {} feedrate is NaN", i);
    }

    // 4. Bounding box присутній і в межах столу (256x256x256 мм)
    let (min, max) = toolpath.bounds().expect("toolpath should have bounds");
    assert!(
        min.x >= 0.0 && max.x <= 256.0,
        "bounds X out of bed: {}..{}",
        min.x,
        max.x
    );
    assert!(
        min.y >= 0.0 && max.y <= 256.0,
        "bounds Y out of bed: {}..{}",
        min.y,
        max.y
    );
    assert!(
        min.z >= 0.0 && max.z <= 256.0,
        "bounds Z out of range: {}..{}",
        min.z,
        max.z
    );
}

#[test]
fn test_ducksie_real_sample() {
    let Some(root) = samples_root() else {
        eprintln!("Set SLICENV_GCODE_SAMPLES to test real Ducksie exports");
        return;
    };
    let path = root.join("Ducksie_EasyWheel-plate1/plate_1.gcode");
    if !path.exists() {
        eprintln!("Ducksie sample not found at {}, skipping", path.display());
        return;
    }

    let started = Instant::now();
    let toolpath = parse_file(&path).expect("failed to parse Ducksie G-code");
    let elapsed = started.elapsed();

    println!(
        "Parsed {} in {:.1} ms: {} layers, {} segments",
        path.display(),
        elapsed.as_secs_f64() * 1000.0,
        toolpath.layers.len(),
        toolpath.segments.len()
    );

    assert_eq!(
        toolpath.stats.total_layer_number,
        Some(226),
        "total_layer_number in header should be 226"
    );
    assert_eq!(
        toolpath.layers.len(),
        226,
        "parsed layer count must match header"
    );

    let mut prev_z = -1.0f32;
    for (i, layer) in toolpath.layers.iter().enumerate() {
        assert!(!layer.z.is_nan(), "layer {} z is NaN", i);
        assert!(!layer.height.is_nan(), "layer {} height is NaN", i);
        assert!(layer.z > 0.0, "layer {} z is not positive: {}", i, layer.z);
        assert!(
            layer.z >= prev_z,
            "layer {} z decreased from {} to {}",
            i,
            prev_z,
            layer.z
        );
        prev_z = layer.z;
    }

    for (i, seg) in toolpath.segments.iter().enumerate() {
        assert!(!seg.from.is_nan(), "seg {} from is NaN", i);
        assert!(!seg.to.is_nan(), "seg {} to is NaN", i);
    }
}
