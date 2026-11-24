fn main() -> std::io::Result<()> {
    const ICON: &str = "../../assets/copiago.ico";

    println!("cargo:rerun-if-changed=copiago.manifest");
    println!("cargo:rerun-if-changed={ICON}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set("ProductName", "CopiaGo")
            .set("FileDescription", "CopiaGo")
            .set("OriginalFilename", "copiago.exe")
            .set_icon(ICON)
            .set_manifest_file("copiago.manifest")
            .compile_for("copiago")?;
    }

    Ok(())
}
