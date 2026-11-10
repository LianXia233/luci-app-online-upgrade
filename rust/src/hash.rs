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
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    Ok(hex)
}

/// Parse SHA-256 hash for `target_file` from checksum text (e.g. `sha256sums` file content or release body).
///
/// File-name matching is case-insensitive and basename based: release
/// checksum lists frequently uppercase the name or prefix it with `./`.
pub fn find_sha256_in_text(text: &str, target_file: &str) -> Option<String> {
    let re_hash = regex::Regex::new(r"(?i)\b([0-9a-f]{64})\b").ok()?;
    let target = basename(target_file).to_ascii_lowercase();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains(&target) {
            if let Some(caps) = re_hash.captures(line) {
                if let Some(m) = caps.get(1) {
                    return Some(m.as_str().to_lowercase());
                }
            }
        }
    }
    None
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Fallback parser for single-hash files (e.g. `xxx.bin.sha256`) OR for a
/// checksum list that contains exactly one distinct hash. A multi-entry list
/// must never fall back to "the first hash": that would compare the firmware
/// against an unrelated file and produce a permanent mismatch.
pub fn extract_single_sha256(text: &str) -> Option<String> {
    let re_hash = regex::Regex::new(r"(?i)\b([0-9a-f]{64})\b").ok()?;
    let mut found: Option<String> = None;
    for caps in re_hash.captures_iter(text) {
        let h = caps.get(1)?.as_str().to_lowercase();
        match &found {
            None => found = Some(h),
            Some(prev) if *prev == h => {}
            Some(_) => return None, // more than one distinct hash: ambiguous
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_sha256() {
        let content = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *H5000M-.-sysupgrade.bin\n1111111111111111111111111111111111111111111111111111111111111111 other.bin";
        let h = find_sha256_in_text(content, "H5000M-.-sysupgrade.bin");
        assert_eq!(
            h,
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into())
        );
    }

    #[test]
    fn test_single_sha256() {
        let content = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n";
        let h = extract_single_sha256(content);
        assert_eq!(
            h,
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into())
        );
    }

    #[test]
    fn ambiguous_list_is_not_reduced_to_the_first_hash() {
        // A multi-entry checksum list must never fall back to "the first hash":
        // that would compare the firmware against an unrelated file.
        let content = "1111111111111111111111111111111111111111111111111111111111111111  a.bin\n\
2222222222222222222222222222222222222222222222222222222222222222  b.bin\n";
        assert_eq!(extract_single_sha256(content), None);
        // same hash repeated (e.g. body quoted twice) is still unambiguous
        let same = "1111111111111111111111111111111111111111111111111111111111111111  a.bin\n\
1111111111111111111111111111111111111111111111111111111111111111  a.bin\n";
        assert_eq!(
            extract_single_sha256(same),
            Some("1111111111111111111111111111111111111111111111111111111111111111".into())
        );
    }

    #[test]
    fn name_match_is_case_insensitive_and_basename_based() {
        let content = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855  ./MyFirmware.IMG.GZ\n";
        assert_eq!(
            find_sha256_in_text(content, "myfirmware.img.gz").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }
}
