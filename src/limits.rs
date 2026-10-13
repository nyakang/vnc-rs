use crate::{Rect, VncError};
use tokio::io::{AsyncRead, AsyncReadExt};

pub(crate) const MAX_PIXELS: usize = 8_294_400;
pub(crate) const MAX_DIMENSION: u16 = 8192;
pub(crate) const MAX_COMPRESSED: usize = 64 * 1024 * 1024;
pub(crate) const MAX_TEXT: usize = 1024 * 1024;
pub(crate) const MAX_NAME: usize = 4096;

pub(crate) fn dimensions(width: u16, height: u16) -> Result<(), VncError> {
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || usize::from(width) * usize::from(height) > MAX_PIXELS
    {
        return Err(VncError::InvalidImageData);
    }
    Ok(())
}

pub(crate) fn rectangle(rect: &Rect, screen: (u16, u16)) -> Result<(), VncError> {
    dimensions(rect.width, rect.height)?;
    if u32::from(rect.x) + u32::from(rect.width) > u32::from(screen.0)
        || u32::from(rect.y) + u32::from(rect.height) > u32::from(screen.1)
    {
        return Err(VncError::InvalidImageData);
    }
    Ok(())
}

pub(crate) async fn string<S: AsyncRead + Unpin>(
    reader: &mut S,
    limit: usize,
) -> Result<String, VncError> {
    let length = reader.read_u32().await? as usize;
    if length > limit {
        return Err(VncError::General("VNC string exceeds size limit".into()));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Resource limits applied to all server-controlled RFB values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VncLimits {
    pub max_server_name_bytes: usize,
    pub max_failure_reason_bytes: usize,
    pub min_ra2_key_bits: u32,
    pub max_ra2_key_bits: u32,
    pub max_ra2_record_bytes: usize,
    pub max_clipboard_bytes: usize,
    pub max_framebuffer_width: u16,
    pub max_framebuffer_height: u16,
    pub max_framebuffer_pixels: usize,
    pub max_rectangles_per_update: u16,
    pub max_encoded_payload_bytes: usize,
    pub max_decoded_payload_bytes: usize,
    pub channel_capacity: usize,
}

impl Default for VncLimits {
    fn default() -> Self {
        Self {
            max_server_name_bytes: 64 * 1024,
            max_failure_reason_bytes: 64 * 1024,
            // RFB RSA-AES does not require a 2048-bit server key. Keep the
            // TigerVNC/RealVNC-compatible 1024..=8192 acceptance range for
            // existing Raspberry Pi servers (NyaTerm issue #625). Our client
            // still generates 2048-bit keys; do not infer RSA size from AES-256.
            // https://github.com/TigerVNC/tigervnc/blob/master/common/rfb/CSecurityRSAAES.cxx
            min_ra2_key_bits: 1_024,
            max_ra2_key_bits: 8_192,
            max_ra2_record_bytes: usize::from(u16::MAX),
            max_clipboard_bytes: 16 * 1024 * 1024,
            max_framebuffer_width: 16_384,
            max_framebuffer_height: 16_384,
            max_framebuffer_pixels: 64 * 1024 * 1024,
            max_rectangles_per_update: 4_096,
            max_encoded_payload_bytes: 64 * 1024 * 1024,
            max_decoded_payload_bytes: 256 * 1024 * 1024,
            channel_capacity: 32,
        }
    }
}
