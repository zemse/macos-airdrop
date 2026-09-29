//! Decodes what receivers advertise: the TXT `flags` bitmask and `ReceiverMediaCapabilities`.

use serde::Serialize;
use serde_json::Value;

/// `flags` bits, named as in OpenDrop's reverse engineering (`AirDropReceiverFlags`).
const FEATURES: &[(u64, &str, &str)] = &[
    (0x001, "url", "links"),
    (0x002, "dvzip", "DVZIP archives"),
    (0x004, "pipelining", "pipelining"),
    (0x008, "mixed_types", "mixed item types"),
    (0x040, "iris", "iris"),
    (0x080, "discover", "discover"),
    (0x200, "asset_bundle", "asset bundles"),
];

/// The decoded TXT `flags` value.
#[derive(Debug, Clone, Serialize)]
pub struct Features {
    pub flags: u64,
    pub hex: String,
    /// Names of the known bits that are set.
    pub known: Vec<&'static str>,
    /// Set bits with no known meaning, as hex.
    pub unknown_bits: Option<String>,
}

impl Features {
    pub fn decode(flags: u64) -> Self {
        let known_mask = FEATURES.iter().fold(0, |m, (bit, ..)| m | bit);
        let unknown = flags & !known_mask;
        Self {
            flags,
            hex: format!("{flags:#x}"),
            known: FEATURES
                .iter()
                .filter(|(bit, ..)| flags & bit != 0)
                .map(|(_, name, _)| *name)
                .collect(),
            unknown_bits: (unknown != 0).then(|| format!("{unknown:#x}")),
        }
    }

    pub fn describe(&self) -> Vec<&'static str> {
        FEATURES
            .iter()
            .filter(|(bit, ..)| self.flags & bit != 0)
            .map(|(.., label)| *label)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoCodec {
    /// FourCC, e.g. `hvc1`.
    pub fourcc: String,
    pub name: String,
    pub hardware: bool,
    /// HEVC profiles, when listed.
    pub profiles: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Media {
    pub video_codecs: Vec<VideoCodec>,
    pub hdr: bool,
    /// Dolby Vision profiles, e.g. `["05", "08"]`.
    pub dolby_vision: Vec<String>,
    /// HEIF family image formats, e.g. `heic`, `avif`.
    pub image_formats: Vec<String>,
    pub live_photo_version: Option<String>,
    pub asset_bundle_version: Option<String>,
}

const CODEC_ORDER: &[&str] = &[
    "hvc1", "avc1", "av01", "apco", "apcs", "apcn", "apch", "ap4h", "ap4x",
];

fn codec_name(fourcc: &str) -> String {
    match fourcc {
        "hvc1" => "HEVC",
        "avc1" => "H.264",
        "av01" => "AV1",
        "apco" => "ProRes 422 Proxy",
        "apcs" => "ProRes 422 LT",
        "apcn" => "ProRes 422",
        "apch" => "ProRes 422 HQ",
        "ap4h" => "ProRes 4444",
        "ap4x" => "ProRes 4444 XQ",
        other => other,
    }
    .to_owned()
}

impl Media {
    /// Parses the JSON document in `ReceiverMediaCapabilities`.
    pub fn parse(caps: &Value) -> Self {
        let codecs = &caps["Codecs"];
        let support = &codecs["CodecSupport"];
        let mut video_codecs: Vec<VideoCodec> = support["VTCodecSupportDict"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(fourcc, info)| VideoCodec {
                fourcc: fourcc.clone(),
                name: codec_name(fourcc),
                hardware: info["VTIsHardwareAccelerated"]
                    .as_bool()
                    .unwrap_or_else(|| {
                        info["VTPerProfileSupport"].as_object().is_some_and(|p| {
                            p.values()
                                .any(|v| v["VTIsHardwareAccelerated"] == Value::Bool(true))
                        })
                    }),
                profiles: info["VTSupportedProfiles"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .collect(),
            })
            .collect();
        // Known codecs in CODEC_ORDER (ProRes by quality), unknown ones after, by name.
        video_codecs.sort_by_key(|c| {
            let rank = CODEC_ORDER.iter().position(|f| *f == c.fourcc);
            (rank.unwrap_or(usize::MAX), c.fourcc.clone())
        });
        let strings = |v: &Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        };
        let mut image_formats: Vec<String> = caps["ContainerFormats"]
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(_, f)| strings(&f["HeifSubtypes"]))
            .map(|s| s.trim_start_matches("public.").to_owned())
            .collect();
        image_formats.sort();
        let vendor = &caps["Vendor"]["com.apple"];
        Self {
            video_codecs,
            hdr: support["VTIsHDRAllowedOnDevice"].as_bool().unwrap_or(false),
            dolby_vision: strings(&codecs["hvc1"]["Profiles"]["VTDoViSupportedProfiles"]),
            image_formats,
            live_photo_version: vendor["LivePhotoFormatVersion"].as_str().map(str::to_owned),
            asset_bundle_version: vendor["AssetBundleFormatVersion"]
                .as_str()
                .map(str::to_owned),
        }
    }

    /// One line per aspect, for human output.
    pub fn describe(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let mut video: Vec<String> = Vec::new();
        let mut prores: Vec<&str> = Vec::new();
        for c in &self.video_codecs {
            if let Some(variant) = c.name.strip_prefix("ProRes ") {
                prores.push(variant);
            } else if c.profiles.is_empty() {
                video.push(c.name.clone());
            } else {
                let p: Vec<String> = c.profiles.iter().map(u64::to_string).collect();
                let hw = if c.hardware { ", hardware" } else { "" };
                video.push(format!("{} (profiles {}{hw})", c.name, p.join(",")));
            }
        }
        if !prores.is_empty() {
            video.push(format!("ProRes {}", prores.join(", ")));
        }
        if !video.is_empty() {
            out.push(("video", video.join("; ")));
        }
        let mut hdr = Vec::new();
        if self.hdr {
            hdr.push("HDR".to_owned());
        }
        if !self.dolby_vision.is_empty() {
            hdr.push(format!(
                "Dolby Vision profiles {}",
                self.dolby_vision.join(", ")
            ));
        }
        if !hdr.is_empty() {
            out.push(("hdr", hdr.join("; ")));
        }
        if !self.image_formats.is_empty() {
            let f: Vec<String> = self
                .image_formats
                .iter()
                .map(|s| s.to_uppercase())
                .collect();
            out.push(("images", f.join(", ")));
        }
        let mut photos = Vec::new();
        if let Some(v) = &self.live_photo_version {
            photos.push(format!("Live Photos v{v}"));
        }
        if let Some(v) = &self.asset_bundle_version {
            photos.push(format!("asset bundles v{v}"));
        }
        if !photos.is_empty() {
            out.push(("photos", photos.join(", ")));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_default_flags() {
        // Seen on a MacBook Air running macOS 27; 0x3fb is sharingd's default node flags.
        let f = Features::decode(111_611);
        assert_eq!(f.hex, "0x1b3fb");
        assert_eq!(
            f.known,
            [
                "url",
                "dvzip",
                "mixed_types",
                "iris",
                "discover",
                "asset_bundle"
            ]
        );
        assert_eq!(f.unknown_bits.as_deref(), Some("0x1b130"));
    }

    #[test]
    fn media() {
        let caps = serde_json::json!({
            "Codecs": {
                "hvc1": {"Profiles": {"VTDoViSupportedProfiles": ["05", "08"]}},
                "CodecSupport": {
                    "VTIsHDRAllowedOnDevice": true,
                    "VTCodecSupportDict": {
                        "apcn": {"VTIsHardwareAccelerated": true},
                        "hvc1": {
                            "VTSupportedProfiles": [1, 2],
                            "VTPerProfileSupport": {"1": {"VTIsHardwareAccelerated": true}}
                        }
                    }
                }
            },
            "ContainerFormats": {"public.heif-standard": {"HeifSubtypes": ["public.heic", "public.avif"]}},
            "Vendor": {"com.apple": {"LivePhotoFormatVersion": "1"}}
        });
        let m = Media::parse(&caps);
        assert_eq!(m.video_codecs[0].fourcc, "hvc1");
        assert!(m.video_codecs[0].hardware);
        assert_eq!(m.image_formats, ["avif", "heic"]);
        assert_eq!(
            m.describe(),
            [
                (
                    "video",
                    "HEVC (profiles 1,2, hardware); ProRes 422".to_owned()
                ),
                ("hdr", "HDR; Dolby Vision profiles 05, 08".to_owned()),
                ("images", "AVIF, HEIC".to_owned()),
                ("photos", "Live Photos v1".to_owned()),
            ]
        );
    }
}
