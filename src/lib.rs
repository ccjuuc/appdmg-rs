use std::path::Path;
use std::process::Command;
use anyhow::{Result};
use serde::{Serialize, Deserialize};
use tokio::fs;

// Declare submodules
pub mod ds_store;
pub mod ds_store_template;
pub mod macos_alias;

use ds_store::{Entry, write_ds_store};
use macos_alias::AliasInfo;

// ---------------------------
// Struct Definitions
// ---------------------------

#[derive(Serialize, Deserialize, Debug)]
pub struct DmgContent {
    pub x: u32,
    pub y: u32,
    #[serde(rename = "type")]
    pub type_: String,
    pub path: String,
    #[serde(skip)]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DmgWindowSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DmgWindow {
    pub size: DmgWindowSize,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DmgConfig {
    pub title: String,
    pub icon: String,
    pub background: String,
    #[serde(rename = "icon-size")]
    pub icon_size: f64,
    pub window: DmgWindow,
    pub contents: Vec<DmgContent>,
}

// ---------------------------
// Main Builder Function
// ---------------------------

/// Create a DMG file based on the provided configuration.
pub async fn build(config: &DmgConfig, final_dmg_path: &Path) -> Result<()> {
    // 1. Prepare temp directory
    let temp_dir = std::env::temp_dir().join(format!("appdmg_rs_{}", std::process::id()));
    if temp_dir.exists() { fs::remove_dir_all(&temp_dir).await?; }
    fs::create_dir_all(&temp_dir).await?;

    // 2. Copy contents
    for item in &config.contents {
        let src_path = Path::new(&item.path);
        let item_name = item.name.as_deref().or_else(|| src_path.file_name().and_then(|n| n.to_str())).unwrap_or("file");
        let dest_path = temp_dir.join(item_name);

        if item.type_ == "file" {
             let status = Command::new("cp").arg("-R").arg(src_path).arg(&dest_path).status()?;
             if !status.success() { return Err(anyhow::anyhow!("Failed to copy content: {:?}", src_path)); }
        } else if item.type_ == "link" {
             let _ = tokio::fs::symlink(src_path, &dest_path).await;
        }
    }

    // 3. Create HFS+ DMG
    let temp_dmg_path = temp_dir.parent().unwrap().join(format!("temp_rw_{}.dmg", std::process::id()));
    if temp_dmg_path.exists() { fs::remove_file(&temp_dmg_path).await?; }

    let status = Command::new("hdiutil")
        .arg("create").arg("-srcfolder").arg(&temp_dir)
        .arg("-volname").arg(&config.title)
        .arg("-fs").arg("HFS+") 
        .arg("-format").arg("UDRW").arg("-ov")
        .arg(&temp_dmg_path)
        .arg("-quiet")
        .status()?;
    if !status.success() { return Err(anyhow::anyhow!("hdiutil create failed")); }

    // 4. Attach
    let attach_out = Command::new("hdiutil").arg("attach").arg("-readwrite").arg("-noverify").arg("-noautoopen").arg(&temp_dmg_path).output()?;
    let out_str = String::from_utf8_lossy(&attach_out.stdout);
    let mount_point = out_str.lines().find_map(|l| l.split('\t').last().map(|s| s.trim()).filter(|s| s.starts_with("/Volumes/"))).ok_or_else(|| anyhow::anyhow!("No mount point"))?;
    let mount_path = Path::new(mount_point);

    // 5. Layout
    // Background Setup
    let bg_dir = mount_path.join(".background");
    fs::create_dir_all(&bg_dir).await?;
    let bg_src = Path::new(&config.background);
    let vol_bg_path = bg_dir.join("background.png");
    if bg_src.exists() {
        fs::copy(bg_src, &vol_bg_path).await?;
    }
    let _ = Command::new("chflags").arg("hidden").arg(&bg_dir).status();
    let _ = Command::new("chflags").arg("hidden").arg(mount_path.join(".fseventsd")).status();

    // Alias
    let alias_info = AliasInfo::new(&vol_bg_path).ok();
    let bg_alias_data = alias_info.and_then(|i| i.encode().ok());

    // DS_Store Entries
    let mut entries = Vec::new();
    for item in &config.contents {
        let item_name = item.name.as_deref().or_else(|| Path::new(&item.path).file_name().and_then(|n| n.to_str())).unwrap_or("file");
        // Skip Iloc for "license" to let Finder auto-arrange it
        if item_name == "license" { continue; }
        entries.push(Entry::new_iloc(item_name, item.x, item.y));
    }
    
    if let Ok(e) = Entry::new_bwsp(config.window.size.width, config.window.size.height) { entries.push(e); }
    if let Ok(e) = Entry::new_icvp(config.icon_size, bg_alias_data) { entries.push(e); }
    
    write_ds_store(&mount_path.join(".DS_Store"), entries).await?;

    // Volume Icon
    if Path::new(&config.icon).exists() {
         let dest_icon = mount_path.join(".VolumeIcon.icns");
         if let Ok(_) = fs::copy(&config.icon, &dest_icon).await {
             let _ = Command::new("chflags").arg("hidden").arg(&dest_icon).status();
             let _ = Command::new("SetFile").arg("-a").arg("C").arg(mount_path).status();
         }
    }
    
    let _ = Command::new("sync").status();

    // 6. Detach & Convert
    Command::new("hdiutil").arg("detach").arg(mount_point).arg("-force").arg("-quiet").status()?;
    
    if final_dmg_path.exists() { fs::remove_file(final_dmg_path).await?; }
    let status = Command::new("hdiutil").arg("convert").arg(&temp_dmg_path).arg("-format").arg("UDZO").arg("-o").arg(final_dmg_path).arg("-quiet").status()?;
    
    let _ = fs::remove_dir_all(&temp_dir).await;
    let _ = fs::remove_file(&temp_dmg_path).await;
    
    if !status.success() { return Err(anyhow::anyhow!("hdiutil convert failed")); }
    Ok(())
}
