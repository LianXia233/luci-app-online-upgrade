use ring::digest::{Context, SHA256};
use std::fs::File;
use std::io::Read;

/// Compute SHA-256 hex digest of a local file.
pub fn sha256_file(path: &str) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut ctx = Context::new(&SHA256);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let digest = ctx.finish();
    let hex: String = digest.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    Ok(hex)
}

/// Parse SHA-256 hash for `target_file` from checksum text (e.g. `sha256sums` file content or release body).
pub fn find_sha256_in_text(text: &str, target_file: &str) -> Option<String> {
    let re_hash = regex::Regex::new(r"(?i)\b([0-9a-f]{64})\b").ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // If line mentions the target file name
        if line.contains(target_file) {
            if let Some(caps) = re_hash.captures(line) {
                if let Some(m) = caps.get(1) {
                    return Some(m.as_str().to_lowercase());
                }
            }
        }
    }
    None
}

/// Fallback parser if text is a single-hash file (like `xxx.bin.sha256`).
pub fn extract_single_sha256(text: &str) -> Option<String> {
    let re_hash = regex::Regex::new(r"(?i)\b([0-9a-f]{64})\b").ok()?;
    re_hash.captures(text).and_then(|c| c.get(1)).map(|m| m.as_str().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_sha256() {
        let content = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *H5000M-.-sysupgrade.bin\n1111111111111111111111111111111111111111111111111111111111111111 other.bin";
        let h = find_sha256_in_text(content, "H5000M-.-sysupgrade.bin");
        assert_eq!(h, Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()));
    }

    #[test]
    fn test_single_sha256() {
        let content = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n";
        let h = extract_single_sha256(content);
        assert_eq!(h, Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()));
    }
}
