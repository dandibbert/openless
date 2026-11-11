//! Update discovery for the dandibbert/openless fork.
//!
//! GitHub Releases are canonical. A nearby GitHub mirror may accelerate downloads,
//! but this fork does not publish to the upstream CDN. Tauri still verifies the
//! signed artifact and version before installation. The initial switch away from
//! the inherited upstream key requires a manual installation.

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

/// Rolling GitHub-latest JSON used as Android's secondary source. `/releases/latest`
/// does not include prereleases, so Beta typically falls through to tag discovery.
pub fn manifest_url(channel: UpdateChannel, target: &str, arch: &str) -> String {
    let name = match channel {
        UpdateChannel::Stable => format!("latest-{target}-{arch}-mirror.json"),
        UpdateChannel::Beta => format!("latest-{target}-{arch}-beta-mirror.json"),
    };
    format!("{MIRROR_BASE}/releases/latest/download/{name}")
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

    #[test]
    fn android_secondary_manifest_stays_on_the_fork_mirror() {
        let stable = manifest_url(UpdateChannel::Stable, "android", "aarch64");
        assert!(stable.contains("dandibbert/openless"));
        assert!(stable.ends_with("latest-android-aarch64-mirror.json"));
        assert!(!stable.contains("aeoform"));
        let beta = manifest_url(UpdateChannel::Beta, "android", "aarch64");
        assert!(beta.ends_with("latest-android-aarch64-beta-mirror.json"));
    }
}

/// Prefer a nearby mirror when latency and throughput are comparable.
/// Version precedence is resolved before measuring transport performance.
#[cfg(not(mobile))]
pub(crate) fn mirror_is_faster(mirror: (f64, f64), github: (f64, f64)) -> bool {
    mirror.0 <= github.0 * 1.2 && mirror.1 >= github.1 * 0.8
}

#[cfg(not(mobile))]
pub(crate) async fn probe_download(url: &url::Url) -> Option<(f64, f64)> {
    use futures_util::StreamExt;
    let started = std::time::Instant::now();
    let response = crate::net::http()
        .get(url.clone())
        .header(reqwest::header::RANGE, "bytes=0-65535")
        .timeout(std::time::Duration::from_secs(4))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let transfer = std::time::Instant::now();
    let mut stream = response.bytes_stream();
    let first = stream.next().await?.ok()?;
    let latency = started.elapsed().as_secs_f64();
    let mut bytes = first.len().min(65536);
    while bytes < 65536 {
        match stream.next().await {
            Some(Ok(chunk)) => bytes = (bytes + chunk.len()).min(65536),
            Some(Err(_)) => return None,
            None => break,
        }
    }
    (bytes > 0).then(|| {
        (
            latency,
            bytes as f64 / transfer.elapsed().as_secs_f64().max(0.001),
        )
    })
}

#[cfg(not(mobile))]
pub(crate) fn prefer_mirror(mirror: Option<(f64, f64)>, github: Option<(f64, f64)>) -> bool {
    match (mirror, github) {
        (Some(_), None) => true,
        (Some(mirror), Some(github)) => mirror_is_faster(mirror, github),
        _ => false,
    }
}

/// GitHub's release assets remain independent of the rolling CDN manifest.
#[cfg(not(mobile))]
pub(crate) async fn github_manifest(channel: UpdateChannel) -> Result<serde_json::Value, String> {
    match github_api_manifest(channel).await {
        Ok(manifest) => Ok(manifest),
        Err(api) => github_web_manifest(channel)
            .await
            .map_err(|web| format!("GitHub API: {api}; public releases: {web}")),
    }
}

#[cfg(not(mobile))]
async fn github_api_manifest(channel: UpdateChannel) -> Result<serde_json::Value, String> {
    let endpoint = match channel {
        UpdateChannel::Stable => "https://api.github.com/repos/dandibbert/openless/releases/latest",
        UpdateChannel::Beta => {
            "https://api.github.com/repos/dandibbert/openless/releases?per_page=30"
        }
    };
    let client = crate::net::http();
    let value: serde_json::Value = client
        .get(endpoint)
        .header("User-Agent", "OpenLess-Updater")
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let release = if matches!(channel, UpdateChannel::Beta) {
        latest_github_beta(&value).ok_or("no published GitHub beta release")?
    } else {
        &value
    };
    let tag = release["tag_name"]
        .as_str()
        .ok_or("missing GitHub release tag")?;
    let version = tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .strip_suffix("-tauri")
        .ok_or("not a desktop release")?;
    semver::Version::parse(version).map_err(|e| e.to_string())?;
    let arch = std::env::consts::ARCH;
    let assets = release["assets"]
        .as_array()
        .ok_or("missing GitHub release assets")?;
    let artifact = assets
        .iter()
        .find(|asset| {
            let name = asset["name"].as_str().unwrap_or("");
            if cfg!(target_os = "macos") {
                name == format!("OpenLess_{arch}.app.tar.gz")
            } else {
                name.starts_with("OpenLess_") && name.ends_with("_x64-setup.exe")
            }
        })
        .ok_or("no matching signed GitHub updater artifact")?;
    let name = artifact["name"].as_str().ok_or("missing artifact name")?;
    let signature = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(&format!("{name}.sig")))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or("missing GitHub signature")?;
    let signature = client
        .get(signature)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let version = github_signed_version(signature.trim(), version)?;
    Ok(
        serde_json::json!({"version": version, "url": artifact["browser_download_url"],
        "signature": signature.trim(), "notes": release["body"], "pub_date": release["published_at"]}),
    )
}

#[cfg(all(test, not(mobile)))]
mod routing_tests {
    use super::*;
    #[test]
    fn nearby_mirror_wins_ties_but_not_a_faster_github_path() {
        assert!(prefer_mirror(Some((0.1, 100.)), None));
        assert!(!prefer_mirror(None, Some((0.1, 100.))));
        assert!(!prefer_mirror(None, None));
        assert!(mirror_is_faster((0.105, 12_000_000.), (0.1, 12_000_000.)));
        assert!(mirror_is_faster((0.04, 30_000_000.), (0.2, 10_000_000.)));
        assert!(!mirror_is_faster((0.2, 30_000_000.), (0.1, 10_000_000.)));
        assert!(!mirror_is_faster((0.1, 2_000_000.), (0.1, 10_000_000.)));
    }
}

#[cfg(not(mobile))]
fn github_signed_version(signature: &str, tag_version: &str) -> Result<String, String> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(signature)
        .map_err(|e| e.to_string())?;
    let decoded = std::str::from_utf8(&decoded).map_err(|e| e.to_string())?;
    let version = decoded
        .lines()
        .find_map(|line| line.strip_prefix("trusted comment: "))
        .and_then(|line| {
            line.split('\t')
                .find_map(|field| field.strip_prefix("version:"))
        })
        .ok_or("GitHub artifact is missing its signed version")?;
    let signed = semver::Version::parse(version).map_err(|e| e.to_string())?;
    let tag = semver::Version::parse(tag_version).map_err(|e| e.to_string())?;
    if signed.major != tag.major
        || signed.minor != tag.minor
        || signed.patch != tag.patch
        || signed.pre != tag.pre
    {
        return Err("GitHub artifact version does not match the release tag".into());
    }
    Ok(version.to_string())
}

#[cfg(all(test, not(mobile)))]
mod signed_version_tests {
    use super::*;
    use base64::Engine;
    #[test]
    fn github_preserves_signed_build_metadata_and_rejects_a_different_release() {
        let signature = base64::engine::general_purpose::STANDARD.encode(
            "untrusted comment: fixture\nsignature\ntrusted comment: timestamp:1\tfile:app.tar.gz\tversion:2.0.2-Beta.4+build.20261106-abc\nglobal");
        assert_eq!(
            github_signed_version(&signature, "2.0.2-Beta.4").unwrap(),
            "2.0.2-Beta.4+build.20261106-abc"
        );
        assert!(github_signed_version(&signature, "2.0.2-Beta.3").is_err());
        assert!(github_signed_version("bad base64", "2.0.2").is_err());
    }
}

#[cfg(not(mobile))]
fn latest_github_beta(value: &serde_json::Value) -> Option<&serde_json::Value> {
    value
        .as_array()?
        .iter()
        .filter_map(|release| {
            if release["draft"] != false || release["prerelease"] != true {
                return None;
            }
            let tag = release["tag_name"]
                .as_str()?
                .strip_prefix('v')?
                .strip_suffix("-tauri")?;
            Some((semver::Version::parse(tag).ok()?, release))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, release)| release)
}

#[cfg(all(test, not(mobile)))]
mod release_freshness_tests {
    use super::*;
    #[test]
    fn beta_freshness_uses_versions_and_never_drafts_or_stable_releases() {
        let releases = serde_json::json!([
            {"tag_name":"v2.0.2-Beta.9-tauri","draft":false,"prerelease":true,"published_at":"2026-11-07T00:00:00Z"},
            {"tag_name":"v2.0.2-Beta.10-tauri","draft":false,"prerelease":true,"published_at":"2026-11-06T00:00:00Z"},
            {"tag_name":"v2.2.0-tauri","draft":false,"prerelease":false},
            {"tag_name":"v2.3.0-Beta.1-tauri","draft":true,"prerelease":true},
            {"tag_name":"v9.0.0-ios","draft":false,"prerelease":true}
        ]);
        assert_eq!(
            latest_github_beta(&releases).unwrap()["tag_name"],
            "v2.0.2-Beta.10-tauri"
        );
    }
}

/// Public release pages remain usable when an anonymous API quota is exhausted.
/// Only this repository's canonical tags and signed updater assets are accepted.
#[cfg(not(mobile))]
async fn github_web_manifest(channel: UpdateChannel) -> Result<serde_json::Value, String> {
    async fn text(url: &str) -> Result<String, String> {
        crate::net::http()
            .get(url)
            .header("User-Agent", "OpenLess-Updater")
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())
    }
    let feed = text("https://github.com/dandibbert/openless/releases.atom").await?;
    let tag =
        github_feed_tag(&feed, channel).ok_or("no published channel release in GitHub feed")?;
    let version = tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .strip_suffix("-tauri")
        .unwrap();
    let assets = text(&format!(
        "https://github.com/dandibbert/openless/releases/expanded_assets/{tag}"
    ))
    .await?;
    let (artifact, signature) =
        github_page_assets(&assets, tag, std::env::consts::OS, std::env::consts::ARCH)
            .ok_or("no matching signed updater assets on GitHub release page")?;
    let signature = text(&signature).await?;
    let version = github_signed_version(signature.trim(), version)?;
    Ok(
        serde_json::json!({"version": version, "url": artifact, "signature": signature.trim(),
        "notes": format!("https://github.com/dandibbert/openless/releases/tag/{tag}")}),
    )
}

#[cfg(any(not(mobile), target_os = "android", test))]
fn github_feed_tag(feed: &str, channel: UpdateChannel) -> Option<&str> {
    github_feed_tags(feed, channel).into_iter().next()
}

#[cfg(any(not(mobile), target_os = "android", test))]
fn github_feed_tags(feed: &str, channel: UpdateChannel) -> Vec<&str> {
    let mut tags: Vec<_> = feed
        .split("<entry>")
        .skip(1)
        .filter_map(|entry| {
            let entry = entry.split("</entry>").next()?;
            let tag = entry
                .split("href=\"https://github.com/dandibbert/openless/releases/tag/")
                .nth(1)?
                .split('"')
                .next()?;
            let version = semver::Version::parse(
                tag.strip_prefix('v')
                    .unwrap_or(tag)
                    .strip_suffix("-tauri")?,
            )
            .ok()?;
            let matches = match channel {
                UpdateChannel::Stable => version.pre.is_empty(),
                UpdateChannel::Beta => version
                    .pre
                    .as_str()
                    .to_ascii_lowercase()
                    .starts_with("beta."),
            };
            matches.then_some((version, tag))
        })
        .collect();
    tags.sort_by(|a, b| b.0.cmp_precedence(&a.0));
    tags.dedup_by(|a, b| a.1 == b.1);
    tags.into_iter().map(|(_, tag)| tag).collect()
}

#[cfg(not(mobile))]
fn github_page_assets(page: &str, tag: &str, os: &str, arch: &str) -> Option<(String, String)> {
    let prefix = format!("/dandibbert/openless/releases/download/{tag}/");
    let paths: Vec<_> = page
        .split("href=\"")
        .skip(1)
        .filter_map(|part| {
            let href = part.split('"').next()?;
            let name = href.strip_prefix(&prefix)?;
            (!name.contains('/')).then_some((href, name))
        })
        .collect();
    let (artifact, name) = paths.iter().find(|(_, name)| {
        if os == "macos" {
            *name == format!("OpenLess_{arch}.app.tar.gz")
        } else if os == "windows" {
            name.starts_with("OpenLess_") && name.ends_with("_x64-setup.exe")
        } else {
            false
        }
    })?;
    let (signature, _) = paths
        .iter()
        .find(|(_, candidate)| *candidate == format!("{name}.sig"))?;
    Some((
        format!("https://github.com{artifact}"),
        format!("https://github.com{signature}"),
    ))
}

#[cfg(all(test, not(mobile)))]
mod public_release_tests {
    use super::*;
    #[test]
    fn feed_uses_semantic_channel_versions_and_canonical_repository_links() {
        let feed = r#"<entry><link href="https://github.com/dandibbert/openless/releases/tag/v2.0.2-Beta.9-tauri"/></entry>
        <entry><link href="https://github.com/dandibbert/openless/releases/tag/v2.0.2-Beta.10-tauri"/></entry>
        <entry><link href="https://github.com/dandibbert/openless/releases/tag/v2.1.0-tauri"/></entry>
        <entry><link href="https://github.com/other/repo/releases/tag/v9.0.0-Beta.1-tauri"/></entry>
        <entry><link href="https://github.com/dandibbert/openless/releases/tag/not-a-version-tauri"/></entry>"#;
        assert_eq!(
            github_feed_tag(feed, UpdateChannel::Beta),
            Some("v2.0.2-Beta.10-tauri")
        );
        assert_eq!(
            github_feed_tag(feed, UpdateChannel::Stable),
            Some("v2.1.0-tauri")
        );
    }
    #[test]
    fn web_assets_require_architecture_and_signature_in_the_same_release() {
        let page = r#"<a href="/dandibbert/openless/releases/download/v2.0.2-Beta.4-tauri/OpenLess_aarch64.app.tar.gz">
        <a href="/dandibbert/openless/releases/download/v2.0.2-Beta.4-tauri/OpenLess_aarch64.app.tar.gz.sig">
        <a href="/dandibbert/openless/releases/download/v2.0.2-Beta.4-tauri/OpenLess_2.0.2%2BBuild_x64-setup.exe">
        <a href="/dandibbert/openless/releases/download/v2.0.2-Beta.4-tauri/OpenLess_2.0.2%2BBuild_x64-setup.exe.sig">"#;
        let tag = "v2.0.2-Beta.4-tauri";
        assert!(github_page_assets(page, tag, "macos", "aarch64")
            .unwrap()
            .0
            .ends_with("OpenLess_aarch64.app.tar.gz"));
        assert!(github_page_assets(page, tag, "windows", "x86_64")
            .unwrap()
            .0
            .ends_with("_x64-setup.exe"));
        assert!(github_page_assets(page, tag, "macos", "x86_64").is_none());
        assert!(github_page_assets(page, "v2.0.2-Beta.3-tauri", "macos", "aarch64").is_none());
        assert!(github_page_assets(
            &page.replace(".tar.gz.sig", ".tar.gz.unsigned"),
            tag,
            "macos",
            "aarch64"
        )
        .is_none());
    }
}

#[cfg(any(target_os = "android", test))]
mod android;
#[cfg(any(target_os = "android", test))]
#[allow(unused_imports)]
pub(crate) use android::*;
