fn main() {
    // 构建印章：把编译时刻写进二进制。真机排查时「界面显示的构建时间早于修复提交」
    // 能一眼区分「代码没生效」和「测的是旧构建」——这个坑踩过一次。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=HUAJING_BUILD_TS={ts}");
    // build.rs 自身变化也要触发重编
    println!("cargo:rerun-if-changed=build.rs");
    tauri_build::build()
}
