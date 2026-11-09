//! Звірка з парою G-code/result.json Bambu CLI; див. docs/PREVIEW_STATS.md.

#[test]
#[ignore = "потрібні GCODE_TIME_REFERENCE та GCODE_TIME_RESULT із референсною нарізкою"]
fn bambu_time_distribution() {
    let path = std::env::var("GCODE_TIME_REFERENCE").expect("GCODE_TIME_REFERENCE");
    let result = std::env::var("GCODE_TIME_RESULT").expect("GCODE_TIME_RESULT");
    let t = gcode::parse_file(path).unwrap();
    let engine = gcode::EngineReport::read(result.as_ref(), Some(1)).unwrap();
    let total = engine.total.unwrap();
    assert!((t.estimate.total - total).abs() < 3.0);
    assert_eq!(t.estimate.layers.len(), t.layers.len());
    for role in [
        gcode::Role::ExternalPerimeter,
        gcode::Role::Perimeter,
        gcode::Role::GapFill,
        gcode::Role::SolidInfill,
    ] {
        let expected = engine
            .feature_times
            .iter()
            .find(|(name, _)| Some(name.as_str()) == role.bambu_name())
            .unwrap()
            .1;
        let actual = t.estimate.role(role);
        assert!(
            (actual - expected).abs() / expected < 0.01,
            "{role:?}: {actual} проти {expected}"
        );
    }
    let report = t.report(Some(&engine));
    assert_eq!(report.total_time, total);
    assert!(report.seams > 0);
}
