//! Bounded snapshots and content digests; these are not filesystem locks or signatures.
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|_| "无法打开证据文件")?;
    let before = file.metadata().map_err(|_| "无法读取文件元数据")?;
    if !before.is_file() {
        return Err("仅支持普通证据文件".into());
    }
    if before.len() > limit {
        return Err(format!("证据文件超过 {limit} 字节上限"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取证据文件失败")?;
    let after = file.metadata().map_err(|_| "无法复核文件元数据")?;
    if bytes.len() as u64 > limit {
        return Err(format!("证据文件超过 {limit} 字节上限"));
    }
    if before.len() != after.len()
        || before.len() != bytes.len() as u64
        || before.modified().ok() != after.modified().ok()
    {
        return Err("证据文件在读取期间变化，请重新准备".into());
    }
    Ok(bytes)
}

/// A failed write deliberately keeps its reservation. A later attempt must not overwrite it.
pub fn create(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("无法创建证据文件（已有文件不会覆盖）：{e}"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("证据写入未完成，已保留文件，请核对后重新准备：{e}"))
}
