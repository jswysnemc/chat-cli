use crate::error::{AppError, AppResult, EXIT_ARGS};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageImage {
    pub media_type: String,
    pub data: String,
}

impl MessageImage {
    pub fn from_bytes(bytes: &[u8], media_type: &str) -> Self {
        Self {
            media_type: media_type.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

pub fn read_image_inputs(
    image_paths: &[PathBuf],
    clipboard_image: bool,
) -> AppResult<Vec<MessageImage>> {
    let mut images = Vec::new();
    for path in image_paths {
        images.push(read_image_file(path)?);
    }
    if clipboard_image {
        images.push(read_clipboard_image()?);
    }
    Ok(images)
}

pub fn read_image_file(path: &Path) -> AppResult<MessageImage> {
    let bytes = fs::read(path).map_err(|err| {
        AppError::new(
            EXIT_ARGS,
            format!("failed to read image `{}`: {err}", path.display()),
        )
    })?;
    let media_type = detect_image_media_type(path, &bytes).ok_or_else(|| {
        AppError::new(
            EXIT_ARGS,
            format!(
                "unsupported image format for `{}`; supported types: {}",
                path.display(),
                SUPPORTED_IMAGE_MEDIA_TYPES.join(", ")
            ),
        )
    })?;
    Ok(MessageImage::from_bytes(&bytes, media_type))
}

pub fn read_clipboard_image() -> AppResult<MessageImage> {
    try_read_wayland_clipboard_image()
        .or_else(try_read_xclip_clipboard_image)
        .or_else(try_read_pngpaste_clipboard_image)
        .ok_or_else(|| {
            AppError::new(
                EXIT_ARGS,
                "failed to read clipboard image; copy an image first and ensure `wl-paste`, `xclip`, or `pngpaste` is available",
            )
        })
}

pub fn read_clipboard_text() -> AppResult<String> {
    try_read_wayland_clipboard_text()
        .or_else(try_read_xclip_clipboard_text)
        .or_else(try_read_pbpaste_clipboard_text)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            AppError::new(
                EXIT_ARGS,
                "failed to read clipboard text; copy some text first and ensure `wl-paste`, `xclip`, or `pbpaste` is available",
            )
        })
}

fn try_read_wayland_clipboard_image() -> Option<MessageImage> {
    let targets = Command::new("wl-paste")
        .args(["--list-types"])
        .output()
        .ok()?;
    if !targets.status.success() {
        return None;
    }
    let targets_text = String::from_utf8_lossy(&targets.stdout);
    let media_type = SUPPORTED_IMAGE_MEDIA_TYPES
        .iter()
        .find(|media_type| targets_text.lines().any(|line| line.trim() == **media_type))?;
    let output = Command::new("wl-paste")
        .args(["--no-newline", "--type", media_type])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(MessageImage::from_bytes(&output.stdout, media_type))
}

fn try_read_xclip_clipboard_image() -> Option<MessageImage> {
    let targets = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .output()
        .ok()?;
    if !targets.status.success() {
        return None;
    }
    let targets_text = String::from_utf8_lossy(&targets.stdout);
    let media_type = SUPPORTED_IMAGE_MEDIA_TYPES
        .iter()
        .find(|media_type| targets_text.lines().any(|line| line.trim() == **media_type))?;
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", media_type, "-o"])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(MessageImage::from_bytes(&output.stdout, media_type))
}

fn try_read_pngpaste_clipboard_image() -> Option<MessageImage> {
    let output = Command::new("pngpaste").arg("-").output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(MessageImage::from_bytes(&output.stdout, "image/png"))
}

fn try_read_wayland_clipboard_text() -> Option<String> {
    let output = Command::new("wl-paste")
        .args(["--no-newline"])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn try_read_xclip_clipboard_text() -> Option<String> {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn try_read_pbpaste_clipboard_text() -> Option<String> {
    let output = Command::new("pbpaste").output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn detect_image_media_type(path: &Path, bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }

    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
    {
        Some(ext) if ext == "png" => Some("image/png"),
        Some(ext) if ext == "jpg" || ext == "jpeg" => Some("image/jpeg"),
        Some(ext) if ext == "gif" => Some("image/gif"),
        Some(ext) if ext == "bmp" => Some("image/bmp"),
        Some(ext) if ext == "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_image_media_type_uses_magic_bytes() {
        assert_eq!(
            detect_image_media_type(Path::new("a.bin"), b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(
            detect_image_media_type(Path::new("a.bin"), &[0xff, 0xd8, 0xff, 0x00]),
            Some("image/jpeg")
        );
    }

    #[test]
    fn detect_image_media_type_falls_back_to_extension() {
        assert_eq!(
            detect_image_media_type(Path::new("a.webp"), b"not enough"),
            Some("image/webp")
        );
    }
}
