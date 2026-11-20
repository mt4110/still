use super::{directory, inventory, regular_file, Message};
use anyhow::Result;
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

/// Only recognize direct Cargo profile caches belonging to a local manifest.
pub(super) fn cache_root(path: &Path) -> Option<&Path> {
    if !matches!(path.file_name()?.to_str()?, "deps" | "incremental") || !directory(path) {
        return None;
    }
    let profile = path.parent()?;
    let target = profile.parent()?;
    if !matches!(profile.file_name()?.to_str()?, "debug" | "release")
        || target.file_name()? != "target"
        || !directory(profile)
        || !directory(target)
        || !regular_file(&target.parent()?.join("Cargo.toml"))
        || !directory(&profile.join(".fingerprint"))
        || !regular_file(&profile.join(".cargo-lock"))
    {
        return None;
    }
    if !regular_file(&target.join(".rustc_info.json")) {
        let tag = target.join("CACHEDIR.TAG");
        if !regular_file(&tag) {
            return None;
        }
        let mut header = String::new();
        fs::File::open(tag)
            .ok()?
            .take(256)
            .read_to_string(&mut header)
            .ok()?;
        if !header.starts_with("Signature: 8a477f597d28d172789f06886806bc55")
            || !header.contains("cache directory tag created by cargo")
        {
            return None;
        }
    }
    Some(target)
}

fn crate_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn base36(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

fn hex_hash(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn session(value: &str) -> bool {
    let mut parts = value.split('-');
    matches!((parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("s"), Some(timestamp), Some(nonce), None) if base36(timestamp, 10) && base36(nonce, 7))
}

fn incremental(parts: &[&str], is_dir: bool) -> bool {
    let Some((krate, hash)) = parts[0].rsplit_once('-') else {
        return false;
    };
    if !crate_name(krate) || !base36(hash, 13) {
        return false;
    }
    if parts.len() == 1 {
        return is_dir;
    }
    let generation = parts[1];
    if let Some(lock) = generation.strip_suffix(".lock") {
        return parts.len() == 2 && !is_dir && session(lock);
    }
    let Some((key, hash)) = generation.rsplit_once('-') else {
        return false;
    };
    if !session(key) || !base36(hash, 25) {
        return false;
    }
    match parts {
        [_, _] => is_dir,
        [_, _, file] if !is_dir => {
            matches!(
                *file,
                "query-cache.bin" | "work-products.bin" | "dep-graph.bin" | "metadata.rmeta"
            ) || file.strip_suffix(".o").is_some_and(|hash| base36(hash, 25))
        }
        _ => false,
    }
}

fn codegen_object(name: &str) -> bool {
    let Some(body) = name.strip_suffix(".rcgu.o") else {
        return false;
    };
    let Some((unit, code)) = body.split_once('.') else {
        return false;
    };
    if base36(unit, 25) {
        return base36(code, 7);
    }
    let Some((hash, number)) = code.split_once("-cgu.") else {
        return false;
    };
    crate_name(unit)
        // rustc prints the codegen disambiguator without leading zeroes.
        && (1..=16).contains(&hash.len())
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

fn dependency(cache: &Path, file: &str) -> bool {
    let (stem, suffix) = file.split_once('.').unwrap_or((file, ""));
    let Some((krate, hash)) = stem.rsplit_once('-') else {
        return false;
    };
    if !crate_name(krate) || !hex_hash(hash) {
        return false;
    }
    if let Some(id) = suffix
        .strip_prefix("long-type-")
        .and_then(|s| s.strip_suffix(".txt"))
    {
        return !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && regular_file(&cache.join(format!("{stem}.d")));
    }
    match suffix {
        "d" => true,
        "rlib" | "rmeta" | "dylib" | "so" => krate.starts_with("lib"),
        "" | "exe" | "dll" | "pdb" | "dll.lib" | "dll.exp" => {
            regular_file(&cache.join(format!("{stem}.d")))
        }
        other => codegen_object(other),
    }
}

pub(super) fn verify_entry(cache: &Path, relative: &Path, metadata: &fs::Metadata) -> Result<()> {
    let unsupported = || {
        Message::new(
            format!(
                "Unrecognized Cargo cache entry: {}",
                inventory::display(relative)
            ),
            format!(
                "Cargo の生成物と確認できない項目を保護: {}",
                inventory::display(relative)
            ),
        )
    };
    if inventory::is_link(metadata) || !(metadata.is_dir() || metadata.is_file()) {
        return Err(unsupported().into());
    }
    if relative.as_os_str().is_empty() && metadata.is_dir() {
        return Ok(());
    }
    let mut parts = [""; 3];
    let mut len = 0;
    for part in relative.components() {
        let Component::Normal(name) = part else {
            return Err(unsupported().into());
        };
        let slot = parts.get_mut(len).ok_or_else(unsupported)?;
        *slot = name.to_str().ok_or_else(unsupported)?;
        len += 1;
    }
    if cache.ends_with("incremental")
        && len == 2
        && metadata.is_dir()
        && parts[1].strip_suffix("-working").is_some_and(session)
    {
        let label = inventory::display(relative);
        return Err(Message::new(
            format!("Contains an unfinished Rust compilation session: {label}"),
            format!("完了を確認できない Rust コンパイル作業域があるため保留: {label}"),
        )
        .into());
    }
    let known = match cache.file_name().and_then(|s| s.to_str()) {
        Some("deps") => metadata.is_file() && len == 1 && dependency(cache, parts[0]),
        Some("incremental") => len > 0 && incremental(&parts[..len], metadata.is_dir()),
        _ => false,
    };
    if !known {
        return Err(unsupported().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_a_marked_profile_and_exact_location() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target");
        let profile = target.join("debug");
        let deps = profile.join("deps");
        fs::create_dir_all(&deps)?;
        fs::create_dir(profile.join(".fingerprint"))?;
        fs::write(profile.join(".cargo-lock"), "")?;
        fs::write(temp.path().join("Cargo.toml"), "[workspace]")?;
        assert!(cache_root(&deps).is_none());
        fs::write(target.join(".rustc_info.json"), "{}")?;
        assert_eq!(cache_root(&deps), Some(target.as_path()));
        assert!(cache_root(&profile).is_none());
        Ok(())
    }

    #[test]
    fn unknown_and_evidence_files_block_partial_cleanup() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let cache = tmp.path().join("deps");
        fs::create_dir(&cache)?;
        let stem = "demo-0123456789abcdef";
        let diag = format!("{stem}.long-type-42.txt");
        assert!(!dependency(&cache, &diag));
        fs::write(cache.join(format!("{stem}.d")), "")?;
        assert!(dependency(&cache, &diag));
        for name in [
            "results.json",
            "foo.log",
            "report.txt",
            "demo-dummy.rlib",
            "demo-0123456789abcdef.test-results.txt",
        ] {
            assert!(!dependency(&cache, name));
        }
        assert!(dependency(
            &cache,
            "security_framework-5ac6dfa4e4b5edff.security_framework.183edbe068cce04-cgu.3.rcgu.o"
        ));
        assert!(!dependency(
            &cache,
            "demo-0123456789abcdef.demo.notes-cgu.2.rcgu.o"
        ));
        let unit = "demo-0123456789abc";
        let generation = "s-ho9wx39w3k-0u5spzg-0xny0l54dee30tk2bxf7bttju";
        assert!(incremental(&[unit, generation, "work-products.bin"], false));
        assert!(!incremental(&[unit, generation, "results.bin"], false));
        assert!(!incremental(&[unit, "notes", "work-products.bin"], false));
        Ok(())
    }
}
