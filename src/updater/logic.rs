use semver::Version;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleaseAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    pub(super) size: u64,
    pub(super) sha256_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleaseCandidate {
    pub(super) tag_name: String,
    pub(super) draft: bool,
    pub(super) prerelease: bool,
    pub(super) assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectedRelease {
    pub(super) version: Version,
    pub(super) asset: ReleaseAsset,
}

pub(super) fn parse_release_version(tag_name: &str) -> Option<Version> {
    let trimmed = tag_name.trim();
    let normalized = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    Version::parse(normalized).ok()
}

pub(super) fn select_update_release(
    current_version: &str,
    releases: &[ReleaseCandidate],
) -> Option<SelectedRelease> {
    let current = Version::parse(current_version).ok()?;

    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = parse_release_version(&release.tag_name)?;
            if version <= current {
                return None;
            }

            let asset = release
                .assets
                .iter()
                .find(|asset| asset.name.to_ascii_lowercase().ends_with(".msi"))?
                .clone();

            Some(SelectedRelease { version, asset })
        })
        .max_by(|left, right| left.version.cmp(&right.version))
}

pub(super) fn cached_download_matches_size(path: &Path, expected_size: u64) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected_size)
}
