//! Build the fixture as native code, once per integration-test process.
use std::{path::{Path, PathBuf}, process::Command, sync::OnceLock};
pub fn executable(name: &str) -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    let directory = BIN.get_or_init(|| {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-fixture");
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/Cargo.toml");
        assert!(Command::new(env!("CARGO")).args(["build","--locked","--manifest-path"]).arg(manifest).arg("--target-dir").arg(&target).status().unwrap().success());
        let built = target.join("debug").join(format!("casimir-test-fixture{}", std::env::consts::EXE_SUFFIX));
        let directory = std::env::temp_dir().join(format!("casimir-fixture-bins-{}",std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["fake-claude","fake-codex","fake-stream","fake-llm","supervisor"] {
            std::fs::copy(&built,directory.join(format!("{name}{}",std::env::consts::EXE_SUFFIX))).unwrap();
        }
        directory
    });
    directory.join(format!("{}{}", name.trim_end_matches(".sh").trim_end_matches(".py"), std::env::consts::EXE_SUFFIX))
}
