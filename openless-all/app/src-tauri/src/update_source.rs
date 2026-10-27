//! Update manifest locations for the dandibbert/openless fork.
//!
//! Upstream moved manifests to dl.aeoform.com, but this fork does not publish
//! to that CDN. Keep Stable on the fork's latest GitHub Release, and Beta on
//! the latest *tagged* prerelease. Each has a mirror followed by a direct URL.
//! Both still use the existing Tauri/minisign signing key.

use crate::types::UpdateChannel;

pub const DIRECT_BASE: &str = "https://github.com/dandibbert/openless";
pub const MIRROR_BASE: &str = "https://fastgit.cc/https://github.com/dandibbert/openless";

pub fn stable_endpoint_templates() -> Vec<String> {
    vec![
        format!("{MIRROR_BASE}/releases/latest/download/latest-{{{{target}}}}-{{{{arch}}}}-mirror.json"),
        format!("{DIRECT_BASE}/releases/latest/download/latest-{{{{target}}}}-{{{{arch}}}}.json"),
    ]
}

pub fn beta_endpoint_templates(tag: &str) -> Vec<String> {
    vec![
        format!("{MIRROR_BASE}/releases/download/{tag}/latest-{{{{target}}}}-{{{{arch}}}}-beta-mirror.json"),
        format!("{DIRECT_BASE}/releases/download/{tag}/latest-{{{{target}}}}-{{{{arch}}}}-beta.json"),
    ]
}

/// Full manifest URLs for Android. The caller must discover the latest Beta
/// tag first; there is no globally rolling Beta manifest in GitHub Releases.
pub fn manifest_urls(
    channel: UpdateChannel,
    target: &str,
    arch: &str,
    beta_tag: Option<&str>,
) -> Vec<String> {
    let templates = match channel {
        UpdateChannel::Stable => stable_endpoint_templates(),
        UpdateChannel::Beta => beta_endpoint_templates(
            beta_tag.expect("Beta requires an explicitly resolved prerelease tag"),
        ),
    };
    templates
        .into_iter()
        .map(|template| {
            template
                .replace("{{target}}", target)
                .replace("{{arch}}", arch)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_manifest_is_scoped_to_fork_release() {
        let urls = manifest_urls(UpdateChannel::Stable, "darwin", "aarch64", None);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("/dandibbert/openless/releases/latest/download/"));
        assert!(urls[0].ends_with("latest-darwin-aarch64-mirror.json"));
        assert!(urls[1].ends_with("latest-darwin-aarch64.json"));
        assert!(!urls[1].contains("-beta"));
    }

    #[test]
    fn beta_manifest_uses_tag_and_beta_suffix() {
        let urls = manifest_urls(UpdateChannel::Beta, "android", "aarch64", Some("v2.0.2-Beta.3-tauri"));
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("/releases/download/v2.0.2-Beta.3-tauri/"));
        assert!(urls[0].ends_with("latest-android-aarch64-beta-mirror.json"));
        assert!(urls[1].ends_with("latest-android-aarch64-beta.json"));
    }

    #[test]
    fn all_supported_target_arch_combinations() {
        for (target, arch) in [
            ("darwin", "aarch64"),
            ("darwin", "x86_64"),
            ("windows", "x86_64"),
            ("android", "aarch64"),
            ("android", "armv7"),
            ("android", "i686"),
            ("android", "x86_64"),
        ] {
            let urls = manifest_urls(UpdateChannel::Stable, target, arch, None);
            assert!(urls.iter().all(|url| url.contains("dandibbert/openless")));
            assert!(urls[1].ends_with(&format!("latest-{target}-{arch}.json")));
        }
    }
}
