//! Durable, content-addressed image attachments like dsh `attachment-local`.
//!
//! Layout below `<home>/attachments/v1` (home = `$DSHX_HOME`, default
//! `~/.dsh-codex`; pass `~/.dsh` to share objects with the official dsh):
//! `objects/<first two hex>/<sha256>` (mode 0400, no extension) staged through
//! `tmp/` and published with a no-overwrite hard link. The id is
//! `sha256:<hex>` of the stored (normalized) bytes.
//!
//! Admission mirrors dsh: the declared media type must match the bytes, the
//! source must fit 20 MiB / 64 MP / 8192 px per side, and an image that is
//! not already a plain single-frame 8-bit sRGB PNG/JPEG/WebP within the
//! normalization budget (4 MP, 8192 px, 4 MiB) is re-encoded: EXIF rotation
//! applied, downscaled to the pixel budget, JPEG at quality 85/75/60 (first
//! that fits) or, with alpha, WebP.
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use dshx_core::{AttachmentStore, Dimensions, ImageAttachmentRef, NewImage};
use image::codecs::{gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder};
use image::{ColorType, DynamicImage, ImageDecoder, metadata::Orientation};
use sha2::{Digest, Sha256};

use crate::error::{IoContext, Result, SessionError};
use crate::lock::create_private_dir;

/// Admission limits and the stored-image normalization budget (dsh defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImagePolicy {
    pub max_image_bytes: usize,
    pub max_image_pixels: u64,
    pub max_image_dimension: u32,
    pub normalized_max_pixels: u64,
    pub normalized_max_dimension: u32,
    pub normalized_max_bytes: usize,
}

impl Default for ImagePolicy {
    fn default() -> Self {
        ImagePolicy {
            max_image_bytes: 20 * 1024 * 1024,
            max_image_pixels: 64_000_000,
            max_image_dimension: 8192,
            normalized_max_pixels: 2048 * 2048,
            normalized_max_dimension: 8192,
            normalized_max_bytes: 4 * 1024 * 1024,
        }
    }
}

const JPEG_QUALITIES: [u8; 3] = [85, 75, 60];

/// Media type from magic bytes (`image/png`, `image/jpeg`, `image/webp`, `image/gif`).
pub fn sniff_media_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// Header facts of a supported image (perceived axes, EXIF rotation applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbedImage {
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
    pub has_alpha: bool,
    /// EXIF, ICC profile or an orientation tag present.
    pub carries_metadata: bool,
    /// Decodes to 8-bit RGB(A).
    pub rgb8: bool,
}

/// Probes media type and dimensions without decoding pixels.
pub fn probe_image(data: &[u8]) -> Result<ProbedImage> {
    with_decoder(data, |decoder, media_type, animated| {
        Ok(probe(decoder, media_type, animated)?.0)
    })
}

fn invalid(detail: impl std::fmt::Display) -> SessionError {
    SessionError::InvalidEvent(format!("unsupported or malformed image data: {detail}"))
}

fn probe(
    decoder: &mut dyn ImageDecoder,
    media_type: &'static str,
    animated: bool,
) -> Result<(ProbedImage, Orientation)> {
    let (w, h) = decoder.dimensions();
    let color = decoder.color_type();
    let icc = decoder.icc_profile().map_err(invalid)?;
    let exif = decoder.exif_metadata().map_err(invalid)?;
    let orientation = decoder.orientation().map_err(invalid)?;
    let transposed = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let probed = ProbedImage {
        media_type,
        width: if transposed { h } else { w },
        height: if transposed { w } else { h },
        animated,
        has_alpha: color.has_alpha(),
        carries_metadata: icc.is_some()
            || exif.is_some()
            || orientation != Orientation::NoTransforms,
        rgb8: matches!(color, ColorType::Rgb8 | ColorType::Rgba8),
    };
    Ok((probed, orientation))
}

/// Runs `f` with the format's decoder (format chosen by magic bytes).
fn with_decoder<T>(
    data: &[u8],
    f: impl FnOnce(&mut dyn ImageDecoder, &'static str, bool) -> Result<T>,
) -> Result<T> {
    let media_type = sniff_media_type(data).ok_or_else(|| invalid("not PNG, JPEG, WebP or GIF"))?;
    let cursor = Cursor::new(data);
    match media_type {
        "image/png" => {
            let mut d = PngDecoder::new(cursor).map_err(invalid)?;
            let animated = d.is_apng().map_err(invalid)?;
            f(&mut d, media_type, animated)
        }
        "image/jpeg" => f(
            &mut JpegDecoder::new(cursor).map_err(invalid)?,
            media_type,
            false,
        ),
        "image/webp" => {
            let mut d = WebPDecoder::new(cursor).map_err(invalid)?;
            let animated = d.has_animation();
            f(&mut d, media_type, animated)
        }
        _ => f(
            &mut GifDecoder::new(cursor).map_err(invalid)?,
            media_type,
            false,
        ),
    }
}

fn decode(data: &[u8]) -> Result<DynamicImage> {
    let media_type = sniff_media_type(data).ok_or_else(|| invalid("not PNG, JPEG, WebP or GIF"))?;
    let cursor = Cursor::new(data);
    let image = match media_type {
        "image/png" => DynamicImage::from_decoder(PngDecoder::new(cursor).map_err(invalid)?),
        "image/jpeg" => DynamicImage::from_decoder(JpegDecoder::new(cursor).map_err(invalid)?),
        "image/webp" => DynamicImage::from_decoder(WebPDecoder::new(cursor).map_err(invalid)?),
        _ => DynamicImage::from_decoder(GifDecoder::new(cursor).map_err(invalid)?),
    };
    image.map_err(invalid)
}

/// dsh `requestImageDimensions`: proportional fit into a pixel budget.
fn budget_dimensions(width: u32, height: u32, max_pixels: u64) -> (u32, u32) {
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = (max_pixels as f64 / (w * h)).sqrt().min(1.0);
    if scale >= 1.0 {
        return (width, height);
    }
    let fits = |a: u32, b: u32| u64::from(a) * u64::from(b) <= max_pixels;
    if width >= height {
        let mut pw = ((w * scale).floor() as u32).max(1);
        let mut ph = ((f64::from(pw) * h / w).round() as u32).max(1);
        while !fits(pw, ph) && pw > 1 {
            pw -= 1;
            ph = ((f64::from(pw) * h / w).round() as u32).max(1);
        }
        (pw, ph)
    } else {
        let mut ph = ((h * scale).floor() as u32).max(1);
        let mut pw = ((f64::from(ph) * w / h).round() as u32).max(1);
        while !fits(pw, ph) && ph > 1 {
            ph -= 1;
            pw = ((f64::from(ph) * w / h).round() as u32).max(1);
        }
        (pw, ph)
    }
}

/// Pixel budget first, then the long-edge cap (dsh `initialDimensions`).
fn normalized_dimensions(width: u32, height: u32, policy: &ImagePolicy) -> (u32, u32) {
    let (w, h) = budget_dimensions(width, height, policy.normalized_max_pixels);
    let long = w.max(h);
    if long <= policy.normalized_max_dimension {
        return (w, h);
    }
    let scale = f64::from(policy.normalized_max_dimension) / f64::from(long);
    (
        ((f64::from(w) * scale).floor() as u32).max(1),
        ((f64::from(h) * scale).floor() as u32).max(1),
    )
}

/// The bytes to store plus their facts.
struct Normalized {
    data: Vec<u8>,
    media_type: &'static str,
    width: u32,
    height: u32,
    original: (u32, u32),
}

fn normalize(data: &[u8], declared: &str, policy: &ImagePolicy) -> Result<Normalized> {
    if data.is_empty() {
        return Err(invalid("image is empty"));
    }
    if data.len() > policy.max_image_bytes {
        return Err(SessionError::InvalidEvent(
            "image exceeds the configured byte limit".into(),
        ));
    }
    let (probed, orientation) = with_decoder(data, probe)?;
    if !declared.is_empty() && declared != probed.media_type {
        return Err(SessionError::InvalidEvent(format!(
            "declared image type {declared} does not match its bytes ({})",
            probed.media_type
        )));
    }
    let (w, h) = (probed.width, probed.height);
    if u64::from(w) * u64::from(h) > policy.max_image_pixels {
        return Err(SessionError::InvalidEvent(
            "image exceeds the configured decoded-pixel limit".into(),
        ));
    }
    if w.max(h) > policy.max_image_dimension {
        return Err(SessionError::InvalidEvent(
            "image exceeds the configured per-side pixel limit".into(),
        ));
    }
    let pass_through = probed.media_type != "image/gif"
        && !probed.animated
        && !probed.carries_metadata
        && probed.rgb8
        && data.len() <= policy.normalized_max_bytes
        && u64::from(w) * u64::from(h) <= policy.normalized_max_pixels
        && w.max(h) <= policy.normalized_max_dimension;
    if pass_through {
        // Admission still proves the bytes decode completely.
        decode(data)?;
        return Ok(Normalized {
            data: data.to_vec(),
            media_type: probed.media_type,
            width: w,
            height: h,
            original: (w, h),
        });
    }
    let mut image = decode(data)?;
    image.apply_orientation(orientation);
    let (tw, th) = normalized_dimensions(image.width(), image.height(), policy);
    if (tw, th) != (image.width(), image.height()) {
        image = image.resize_exact(tw, th, image::imageops::FilterType::Lanczos3);
    }
    let (data, media_type) = if probed.has_alpha {
        encode_webp(&image)?
    } else {
        encode_jpeg_ladder(&image, policy)?
    };
    Ok(Normalized {
        data,
        media_type,
        width: image.width(),
        height: image.height(),
        original: (w, h),
    })
}

fn encode_webp(image: &DynamicImage) -> Result<(Vec<u8>, &'static str)> {
    let rgba = image.to_rgba8();
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| SessionError::InvalidEvent(format!("image normalization failed: {e}")))?;
    Ok((out, "image/webp"))
}

fn encode_jpeg_ladder(
    image: &DynamicImage,
    policy: &ImagePolicy,
) -> Result<(Vec<u8>, &'static str)> {
    let rgb = image.to_rgb8();
    let mut smallest: Option<Vec<u8>> = None;
    for quality in JPEG_QUALITIES {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| SessionError::InvalidEvent(format!("image normalization failed: {e}")))?;
        if out.len() <= policy.normalized_max_bytes {
            return Ok((out, "image/jpeg"));
        }
        if smallest.as_ref().is_none_or(|s| out.len() < s.len()) {
            smallest = Some(out);
        }
    }
    Ok((smallest.unwrap_or_default(), "image/jpeg"))
}

fn hex_digest(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest_of_id(id: &str) -> Result<&str> {
    id.strip_prefix("sha256:")
        .filter(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| {
            SessionError::InvalidEvent(format!("attachment reference {id:?} is invalid"))
        })
}

/// dsh `displayName`: leaf of either separator style, control chars stripped, ≤ 255 chars.
fn display_name(name: Option<&str>) -> Option<String> {
    let name = name?;
    let leaf = &name[name.rfind(['/', '\\']).map_or(0, |i| i + 1)..];
    let clean: String = leaf
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(255)
        .collect();
    (!clean.is_empty()).then_some(clean)
}

/// Local attachment store.
#[derive(Debug, Clone)]
pub struct FileAttachments {
    root: PathBuf,
    policy: ImagePolicy,
}

impl FileAttachments {
    /// Store under `<home>/attachments/v1`.
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self::with_root(home.as_ref().join("attachments").join("v1"))
    }

    /// `$DSHX_HOME/attachments/v1`.
    pub fn open_default() -> Self {
        Self::new(dshx_core::config::home_dir())
    }

    /// Store directly at `root` (the `…/attachments/v1` directory).
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        FileAttachments {
            root: root.into(),
            policy: ImagePolicy::default(),
        }
    }

    pub fn with_policy(mut self, policy: ImagePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of a stored object (dsh `imageHostPath`).
    pub fn object_path(&self, attachment_id: &str) -> Result<PathBuf> {
        let hex = digest_of_id(attachment_id)?;
        Ok(self.root.join("objects").join(&hex[..2]).join(hex))
    }

    /// Validates, normalizes and durably stores one image.
    pub fn save_image_blocking(&self, image: NewImage) -> Result<ImageAttachmentRef> {
        let normalized = normalize(&image.data, &image.media_type, &self.policy)?;
        let hex = hex_digest(&normalized.data);
        let id = format!("sha256:{hex}");
        self.publish(&self.object_path(&id)?, &normalized.data, &hex)?;
        let downscaled = (normalized.width, normalized.height) != normalized.original;
        Ok(ImageAttachmentRef {
            attachment_id: id,
            media_type: normalized.media_type.into(),
            bytes: normalized.data.len() as u64,
            width: normalized.width,
            height: normalized.height,
            name: display_name(image.name.as_deref()),
            original_dimensions: downscaled.then_some(Dimensions {
                width: normalized.original.0,
                height: normalized.original.1,
            }),
        })
    }

    /// Reads an object back, verifying its digest.
    pub fn load_blocking(&self, attachment_id: &str) -> Result<(Vec<u8>, String)> {
        let path = self.object_path(attachment_id)?;
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::NotFound(attachment_id.to_owned()));
            }
            Err(e) => return Err(SessionError::io(&path, e)),
        };
        if hex_digest(&data) != digest_of_id(attachment_id)? {
            return Err(SessionError::Corrupt(format!(
                "stored attachment {attachment_id} failed integrity verification"
            )));
        }
        let media_type = sniff_media_type(&data).ok_or_else(|| {
            SessionError::Corrupt(format!("stored attachment {attachment_id} is not an image"))
        })?;
        Ok((data, media_type.to_owned()))
    }

    /// Stage in `tmp/`, fsync, hard-link into place (an existing object must
    /// match), make it read-only, fsync the directories.
    fn publish(&self, target: &Path, data: &[u8], hex: &str) -> Result<()> {
        let staging = self.root.join("tmp");
        create_private_dir(&staging)?;
        let parent = target.parent().expect("object path has a parent");
        create_private_dir(parent)?;
        let temporary = staging.join(dshx_core::new_id());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options.open(&temporary).and_then(|mut f| {
            f.write_all(data)?;
            f.sync_all()
        });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(SessionError::io(&temporary, e));
        }
        let linked = std::fs::hard_link(&temporary, target);
        let _ = std::fs::remove_file(&temporary);
        match linked {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = std::fs::read(target).at(target)?;
                if hex_digest(&existing) != hex {
                    return Err(SessionError::Corrupt(format!(
                        "stored attachment {} failed integrity verification",
                        target.display()
                    )));
                }
            }
            Err(e) => return Err(SessionError::io(target, e)),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o400)).at(target)?;
            let mut level = parent.to_path_buf();
            while level.starts_with(&self.root) {
                if let Ok(dir) = std::fs::File::open(&level) {
                    let _ = dir.sync_all();
                }
                if !level.pop() {
                    break;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl AttachmentStore for FileAttachments {
    async fn save_image(&self, image: NewImage) -> anyhow::Result<ImageAttachmentRef> {
        let store = self.clone();
        Ok(tokio::task::spawn_blocking(move || store.save_image_blocking(image)).await??)
    }

    async fn load(&self, attachment_id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let store = self.clone();
        let id = attachment_id.to_owned();
        Ok(tokio::task::spawn_blocking(move || store.load_blocking(&id)).await??)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    fn png(width: u32, height: u32, alpha: bool) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        if alpha {
            ImageBuffer::from_fn(width, height, |x, y| {
                Rgba([(x % 256) as u8, (y % 256) as u8, 7, 200])
            })
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        } else {
            ImageBuffer::from_fn(width, height, |x, y| {
                Rgb([(x % 256) as u8, (y % 256) as u8, 7])
            })
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        }
        out.into_inner()
    }

    fn new_image(data: Vec<u8>, media_type: &str) -> NewImage {
        NewImage {
            data,
            media_type: media_type.into(),
            name: Some("C:\\shots\\a.png".into()),
            width: 0,
            height: 0,
        }
    }

    #[test]
    fn sniffing_and_probing() {
        let data = png(30, 20, false);
        assert_eq!(sniff_media_type(&data), Some("image/png"));
        assert_eq!(sniff_media_type(b"nope"), None);
        let probed = probe_image(&data).unwrap();
        assert_eq!(
            (probed.width, probed.height, probed.has_alpha),
            (30, 20, false)
        );
    }

    #[test]
    fn small_plain_image_passes_through_and_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAttachments::new(tmp.path());
        let data = png(40, 30, false);
        let r = store
            .save_image_blocking(new_image(data.clone(), "image/png"))
            .unwrap();
        assert_eq!(r.attachment_id, format!("sha256:{}", hex_digest(&data)));
        assert_eq!((r.width, r.height, r.bytes), (40, 30, data.len() as u64));
        assert_eq!(r.name.as_deref(), Some("a.png"));
        assert_eq!(r.original_dimensions, None);
        let path = store.object_path(&r.attachment_id).unwrap();
        assert!(path.starts_with(tmp.path().join("attachments/v1/objects")));
        let again = store
            .save_image_blocking(new_image(data.clone(), ""))
            .unwrap();
        assert_eq!(again.attachment_id, r.attachment_id);
        let (bytes, media) = store.load_blocking(&r.attachment_id).unwrap();
        assert_eq!((bytes, media.as_str()), (data, "image/png"));
        assert!(
            store
                .save_image_blocking(new_image(png(4, 4, false), "image/jpeg"))
                .is_err()
        );
        assert!(store.load_blocking("sha256:abc").is_err());
    }

    #[test]
    fn large_images_are_downscaled_to_the_pixel_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = ImagePolicy {
            normalized_max_pixels: 100 * 50,
            ..ImagePolicy::default()
        };
        let store = FileAttachments::new(tmp.path()).with_policy(policy);
        let r = store
            .save_image_blocking(new_image(png(400, 200, false), "image/png"))
            .unwrap();
        assert_eq!(r.media_type, "image/jpeg");
        assert_eq!((r.width, r.height), (100, 50));
        assert_eq!(
            r.original_dimensions,
            Some(Dimensions {
                width: 400,
                height: 200
            })
        );
        let r = store
            .save_image_blocking(new_image(png(300, 300, true), "image/png"))
            .unwrap();
        assert_eq!(r.media_type, "image/webp");
        assert!(u64::from(r.width) * u64::from(r.height) <= 5000);
        let (bytes, _) = store.load_blocking(&r.attachment_id).unwrap();
        assert_eq!(probe_image(&bytes).unwrap().width, r.width);
    }

    #[test]
    fn budget_math_matches_dsh() {
        assert_eq!(budget_dimensions(4000, 3000, 2048 * 2048), (2364, 1773));
        assert_eq!(budget_dimensions(100, 100, 2048 * 2048), (100, 100));
        assert_eq!(budget_dimensions(10, 100_000, 10_000), (1, 10_000));
    }
}
