//! A protected certificate in a Cargo target is eligible only if it matches
//! the copy in a published crate identified by the project's lockfile.
use super::Producer;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

const FIXTURE_LIMIT: u64 = 4 * 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 32 * 1024 * 1024;
const EXPANDED_LIMIT: u64 = 256 * 1024 * 1024;
type Digests = HashMap<PathBuf, [u8; 32]>;

pub(super) fn eligible_name(path: &Path) -> bool {
    if path.file_name().is_some_and(|name| name == ".envrc") {
        return true;
    }
    !super::protected_name(
        &path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase(),
    ) && (path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.ends_with(".gpg.key"))
        || matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("der" | "crt" | "cer" | "pem")
        ))
}

pub(super) struct Fixtures<'a> {
    root: &'a Path,
    enabled: bool,
    packages: Option<Vec<Package>>,
    digests: HashMap<String, Digests>,
    cache: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Lock {
    package: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

impl<'a> Fixtures<'a> {
    pub(super) fn new(root: &'a Path, producer: Producer) -> Self {
        Self {
            root,
            enabled: producer == Producer::Cargo,
            packages: None,
            digests: HashMap::new(),
            cache: std::env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join(".cargo")))
                .map(|home| home.join("registry/cache")),
        }
    }

    pub(super) fn digest(&mut self, relative: &Path) -> Option<[u8; 32]> {
        if !self.enabled || !eligible_name(relative) {
            return None;
        }
        let mut parts = relative.strip_prefix("vendor").ok()?.components();
        let Component::Normal(directory) = parts.next()? else {
            return None;
        };
        let directory = directory.to_str()?;
        if !self.digests.contains_key(directory) {
            let packages = self.packages.get_or_insert_with(|| {
                let load = || -> Option<Vec<Package>> {
                    let bytes =
                        limited_file(&self.root.parent()?.join("Cargo.lock"), 8 * 1024 * 1024)
                            .ok()?;
                    Some(
                        toml::from_str::<Lock>(std::str::from_utf8(&bytes).ok()?)
                            .ok()?
                            .package,
                    )
                };
                load().unwrap_or_default()
            });
            let mut matching = packages.iter().filter(|p| {
                p.source.as_deref() == Some("registry+https://github.com/rust-lang/crates.io-index")
                    && (p.name == directory || format!("{}-{}", p.name, p.version) == directory)
            });
            let files = matching.next().and_then(|p| {
                if matching.next().is_some() {
                    return None;
                }
                p.fixtures(self.cache.as_deref()?).ok()
            });
            self.digests
                .insert(directory.to_string(), files.unwrap_or_default());
        }
        let expected = self.digests.get(directory)?.get(parts.as_path())?;
        let data = limited_file(&self.root.join(relative), FIXTURE_LIMIT).ok()?;
        if relative.extension().is_some_and(|ext| ext == "key")
            && !data.starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----")
        {
            return None;
        }
        (Sha256::digest(data).as_slice() == expected).then_some(*expected)
    }
}

fn limited_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(super::regular_file(path), "Expected regular file");
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "Unexpected file size or type"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "File grew during review");
    Ok(bytes)
}

impl Package {
    fn fixtures(&self, archives: &Path) -> Result<Digests> {
        let checksum = self
            .checksum
            .as_ref()
            .context("Missing registry checksum")?;
        let base = format!("{}-{}", self.name, self.version);
        ensure!(
            Path::new(&base)
                .file_name()
                .is_some_and(|name| name == base.as_str()),
            "Invalid crate name"
        );
        for index in std::fs::read_dir(archives)? {
            let archive = index?.path().join(format!("{base}.crate"));
            let Ok(bytes) = limited_file(&archive, ARCHIVE_LIMIT) else {
                continue;
            };
            if format!("{:x}", Sha256::digest(&bytes)) == *checksum {
                return archive_digests(&base, &bytes);
            }
        }
        anyhow::bail!("No matching registry archive")
    }
}

fn archive_digests(base: &str, bytes: &[u8]) -> Result<Digests> {
    let data = flate2::read::GzDecoder::new(Cursor::new(bytes)).take(EXPANDED_LIMIT);
    let mut archive = tar::Archive::new(data);
    let mut digests = Digests::new();
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(index < 64_000, "Registry archive contains too many entries");
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() || entry.size() > FIXTURE_LIMIT {
            continue;
        }
        let path = entry.path()?.strip_prefix(base)?.to_owned();
        if !eligible_name(&path) || !path.components().all(|p| matches!(p, Component::Normal(_))) {
            continue;
        }
        let mut hash = Sha256::new();
        std::io::copy(&mut entry, &mut hash)?;
        ensure!(
            digests.insert(path, hash.finalize().into()).is_none(),
            "Duplicate fixture"
        );
    }
    Ok(digests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::fs;
    use tempfile::TempDir;

    const PEM: &[u8] = b"public crate test fixture";
    const ENVRC: &str = "vendor/example-1.2.3/.envrc";
    const FIXTURE: &str = "vendor/example-1.2.3/tests/sample.der";

    struct Fixture {
        _temp: TempDir,
        cache: PathBuf,
        target: PathBuf,
        archive: PathBuf,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let temp = tempfile::tempdir()?;
            let root = temp.path();
            let target = root.join("target");
            let cache = root.join("registry/cache");
            let index = cache.join("index.crates.io-abc");
            fs::create_dir_all(&index)?;
            let archive = index.join("example-1.2.3.crate");
            let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(PEM.len() as u64);
            header.set_cksum();
            builder.append_data(&mut header, "example-1.2.3/tests/sample.der", PEM)?;
            builder.append_data(&mut header, "example-1.2.3/.envrc", PEM)?;
            builder.append_data(&mut header, "example-1.2.3/.env", PEM)?;
            let bytes = builder.into_inner()?.finish()?;
            let checksum = format!("{:x}", Sha256::digest(&bytes));
            fs::write(&archive, bytes)?;
            fs::write(root.join("Cargo.lock"), format!(
                "[[package]]\nname='example'\nversion='1.2.3'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{checksum}'\n"
            ))?;
            let file = target.join(FIXTURE);
            fs::create_dir_all(file.parent().unwrap())?;
            fs::write(file, PEM)?;
            fs::write(target.join(ENVRC), PEM)?;
            fs::write(target.join("vendor/example-1.2.3/.env"), PEM)?;
            Ok(Self {
                _temp: temp,
                target,
                cache,
                archive,
            })
        }

        fn matches(&self, producer: Producer, path: &str) -> bool {
            let mut review = Fixtures::new(&self.target, producer);
            review.cache = Some(self.cache.clone());
            review.digest(Path::new(path)).is_some()
        }
    }

    #[test]
    fn allows_only_the_published_bytes_at_the_published_path_in_cargo_target() -> Result<()> {
        let fixture = Fixture::new()?;
        assert!(fixture.matches(Producer::Cargo, FIXTURE));
        assert!(!fixture.matches(Producer::DeclaredOutput, FIXTURE));
        let other = "vendor/example-1.2.3/tests/private.der";
        fs::write(fixture.target.join(other), PEM)?;
        assert!(!fixture.matches(Producer::Cargo, other));
        fs::write(fixture.target.join(FIXTURE), b"locally changed fixture")?;
        assert!(!fixture.matches(Producer::Cargo, FIXTURE));
        Ok(())
    }

    #[test]
    fn published_envrc_is_eligible_but_local_edits_and_dotenv_are_held() -> Result<()> {
        let fixture = Fixture::new()?;
        assert!(fixture.matches(Producer::Cargo, ENVRC));
        assert!(!fixture.matches(Producer::Cargo, "vendor/example-1.2.3/.env"));
        let path = fixture.target.join(ENVRC);
        fs::write(&path, b"user content added after checking the archive")?;
        assert!(!fixture.matches(Producer::Cargo, ENVRC));
        let published = Sha256::digest(PEM).into();
        assert!(crate::review::filesystem::stamp(
            &path,
            &path.symlink_metadata()?,
            Some(published)
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn missing_or_changed_archive_fails_closed() -> Result<()> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.archive, b"wrong archive")?;
        assert!(!fixture.matches(Producer::Cargo, FIXTURE));
        fs::rename(&fixture.archive, fixture.archive.with_extension("bak"))?;
        assert!(!fixture.matches(Producer::Cargo, FIXTURE));
        Ok(())
    }

    #[test]
    fn vendor_paths_without_registry_lock_remain_protected() -> Result<()> {
        let fixture = Fixture::new()?;
        fs::write(
            fixture.target.parent().unwrap().join("Cargo.lock"),
            "package=[]\n",
        )?;
        assert!(!fixture.matches(Producer::Cargo, FIXTURE));
        assert!(!eligible_name(Path::new(".env.pem")));
        assert!(!eligible_name(Path::new("private.key")));
        Ok(())
    }
}
