// Embed the application icon and version info into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let icon = std::path::Path::new(&dir).join("assets").join("icon.ico");
    let icon = icon.to_string_lossy().replace('\\', "/");
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = ver.split('.').collect();
    let (a, b, c) = (parts[0], parts.get(1).unwrap_or(&"0"), parts.get(2).unwrap_or(&"0"));
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "InpLite"
      VALUE "FileDescription", "InpLite - lightweight Abaqus input file editor"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "InpLite"
      VALUE "OriginalFilename", "InpLite.exe"
      VALUE "ProductName", "InpLite"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("inplite.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
