use std::fs;

fn stl() -> &'static str {
    "solid test\nfacet normal 0 0 1\nouter loop\nvertex 10 20 5\nvertex 12 20 5\nvertex 10 23 5\nendloop\nendfacet\nendsolid test\n"
}

#[test]
fn stl_decoder_preserves_coordinates_while_viewer_seats_mesh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("triangle.stl");
    fs::write(&path, stl()).unwrap();
    let raw = project::read_stl(&path).unwrap();
    assert_eq!(raw.triangles.len(), 1);
    assert!(raw.positions.contains(&[10., 20., 5.]));
    let mesh = viewer::load_stl(&path).unwrap();
    assert_eq!(mesh.triangle_count(), 1);
    let (min, max) = mesh.bounds().unwrap();
    assert_eq!(min.z, 0.);
    assert_eq!((max - min).to_array(), [2., 3., 0.]);
}

#[test]
fn viewer_uses_document_plate_layout_for_3mf() {
    let dir = tempfile::tempdir().unwrap();
    let stl_path = dir.path().join("triangle.stl");
    fs::write(&stl_path, stl()).unwrap();
    let mut document = project::Project::load(&stl_path).unwrap();
    document.add_plate("Second").unwrap();
    let path = dir.path().join("plates.3mf");
    document.save_3mf(&path).unwrap();
    let plates = viewer::load_model(&path).unwrap();
    assert_eq!(plates.len(), 2);
    assert_eq!(plates[0].number, 1);
    assert_eq!(plates[0].mesh.triangle_count(), 1);
    assert_eq!(plates[1].number, 2);
    assert_eq!(plates[1].name.as_deref(), Some("Second"));
    assert!(plates[1].mesh.positions.is_empty());
}
