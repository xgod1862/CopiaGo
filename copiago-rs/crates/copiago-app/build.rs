fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=copiago.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set("ProductName", "CopiaGo")
            .set("FileDescription", "CopiaGo")
            .set("OriginalFilename", "copiago.exe")
            .set_manifest_file("copiago.manifest")
            .compile_for("copiago")?;
    }

    Ok(())
}
