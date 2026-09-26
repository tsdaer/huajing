//! 版本号三处一致性（M4.0 定版纪律）：tauri.conf.json / package.json / Cargo.toml
//! 必须同版——发布安装包与更新清单读的是 tauri.conf.json，漂移会让更新器
//! 对不上号。漂移在测试层即红，不再靠发布清单人肉对齐。

use serde_json::Value;

#[test]
fn versions_match_across_three_manifests() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let cargo_toml = std::fs::read_to_string(format!("{manifest}/Cargo.toml")).unwrap();
    let cargo_version = cargo_toml
        .lines()
        .find_map(|l| l.strip_prefix("version = \"").map(|r| r.trim_end_matches('"').to_string()))
        .expect("Cargo.toml 缺 version 行");

    let tauri: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{manifest}/tauri.conf.json")).unwrap(),
    )
    .unwrap();
    let tauri_version = tauri["version"].as_str().expect("tauri.conf.json 缺 version").to_string();

    let pkg: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{manifest}/../package.json")).unwrap(),
    )
    .unwrap();
    let pkg_version = pkg["version"].as_str().expect("package.json 缺 version").to_string();

    assert_eq!(cargo_version, tauri_version, "Cargo.toml 与 tauri.conf.json 版本不一致");
    assert_eq!(cargo_version, pkg_version, "Cargo.toml 与 package.json 版本不一致");
}
