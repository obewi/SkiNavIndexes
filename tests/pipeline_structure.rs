use std::{fs, path::PathBuf};

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn pipeline_is_split_into_focused_modules() {
    let pipeline_entrypoint =
        fs::read_to_string(manifest_path("src/pipeline.rs")).expect("read src/pipeline.rs");
    let entrypoint_lines = pipeline_entrypoint.lines().count();
    assert!(
        entrypoint_lines <= 180,
        "src/pipeline.rs should stay a thin orchestration module, found {entrypoint_lines} lines"
    );

    for module in [
        "build.rs",
        "fetch.rs",
        "geo.rs",
        "io.rs",
        "model.rs",
        "normalize.rs",
        "output.rs",
        "release.rs",
        "validate.rs",
    ] {
        assert!(
            manifest_path(&format!("src/pipeline/{module}")).exists(),
            "missing focused pipeline module src/pipeline/{module}"
        );
    }
}
