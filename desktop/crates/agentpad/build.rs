fn main() {
    println!("cargo:rerun-if-env-changed=AGENTPAD_VERSION");
    println!("cargo:rerun-if-env-changed=AGENTPAD_BUILD_ID");
    println!("cargo:rerun-if-env-changed=AGENTPAD_CHANNEL");
    let build_id = std::env::var("AGENTPAD_BUILD_ID").unwrap_or_else(|_| {
        std::process::Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "local".into())
    });
    println!("cargo:rustc-env=AGENTPAD_BUILD_ID={build_id}");
    println!(
        "cargo:rustc-env=AGENTPAD_CHANNEL={}",
        std::env::var("AGENTPAD_CHANNEL").unwrap_or_else(|_| "stable".into())
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = std::fs::read("agentpad-icon.png").expect("failed to read Windows icon");
        let version_text = std::env::var("AGENTPAD_VERSION")
            .unwrap_or_else(|_| std::env::var("CARGO_PKG_VERSION").unwrap());
        let version = parse_version(&version_text);
        embedinator::ResourceBuilder::from_env()
            .set_file_version(version)
            .set_product_version(version)
            .add_string("FileVersion", &version_text)
            .add_string("ProductVersion", &version_text)
            .add_string("ProductName", "AgentsPads")
            .add_string("FileDescription", "AgentsPads")
            .add_manifest(std::fs::read_to_string("app.manifest").expect("Windows manifest"))
            .add_icon(32512, embedinator::Icon::from_png_bytes(icon))
            .finish();
        println!("cargo:rerun-if-changed=agentpad-icon.png");
        println!("cargo:rerun-if-changed=app.manifest");
    }
}

fn parse_version(version: &str) -> embedinator::Version {
    let mut parts = version.split('.').map(|part| {
        part.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u16>()
            .unwrap_or(0)
    });
    embedinator::Version::new(
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        0,
    )
}
