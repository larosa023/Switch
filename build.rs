use std::{env, fs, path::PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ver = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u32> = ver
        .split(|c| c == '.' || c == '-')
        .filter_map(|p| p.parse().ok())
        .collect();
    parts.resize(4, 0);
    let commas = parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let p = |f: &str| root.join("assets").join(f).to_string_lossy().replace('\\', "/");

    let rc = format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEFLAGSMASK 0x3f
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "larosa023"
      VALUE "FileDescription", "Switch"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "Switch"
      VALUE "OriginalFilename", "Switch.exe"
      VALUE "ProductName", "Switch"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = p("switch.ico"),
        manifest = p("switch.manifest"),
    );
    let rc_path = out.join("switch.rc");
    fs::write(&rc_path, rc).unwrap();
    println!("cargo:rerun-if-changed=assets/switch.ico");
    println!("cargo:rerun-if-changed=assets/switch.manifest");
    println!("cargo:rerun-if-changed=Cargo.toml");
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
