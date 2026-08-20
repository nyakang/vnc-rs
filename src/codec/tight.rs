use crate::{PixelFormat, Rect, VncError, VncEvent, VncLimits};
use image::{GenericImageView, ImageReader, Limits as ImageLimits};
use std::future::Future;
use std::io::Cursor;
use std::io::Read;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::error;

use super::{
    alpha_shift, checked_pixel_count, checked_rgb_size, checked_rgba_size, ensure_payload_limit,
    rgb_to_pixel, uninit_vec, zlib::ZlibReader,
};

const MAX_PALETTE: usize = 256;

#[derive(Default)]
pub struct Decoder {
    zlibs: [Option<flate2::Decompress>; 4],
    ctrl: u8,
    filter: u8,
    palette: Vec<u8>,
    alpha_shift: u32,
    limits: VncLimits,
}

impl Decoder {
    pub fn new(limits: VncLimits) -> Self {
        let mut new = Self {
            palette: Vec::with_capacity(MAX_PALETTE * 4),
            limits,
            ..Default::default()
        };
        for i in 0..4 {
            let decompressor = flate2::Decompress::new(true);
            new.zlibs[i] = Some(decompressor);
        }
        new
    }

    pub async fn decode<S, F, Fut>(
        &mut self,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        self.alpha_shift = alpha_shift(format)?;

        let ctrl = input.read_u8().await?;
        for i in 0..4 {
            if (ctrl >> i) & 1 == 1 {
                self.zlibs[i]
                    .as_mut()
                    .ok_or(VncError::InvalidImageData)?
                    .reset(true);
            }
        }

        // Figure out filter
        self.ctrl = ctrl >> 4;

        match self.ctrl {
            8 => {
                // fill Rect
                self.fill_rect(format, rect, input, output_func).await
            }
            9 => {
                // jpeg Rect
                self.jpeg_rect(format, rect, input, output_func).await
            }
            10 => {
                // png Rect
                error!("PNG received in standard Tight rect");
                Err(VncError::InvalidImageData)
            }
            x if x & 0x8 == 0 => {
                // basic Rect
                self.basic_rect(format, rect, input, output_func).await
            }
            _ => {
                error!("Illegal tight compression received ({})", self.ctrl);
                Err(VncError::InvalidImageData)
            }
        }
    }

    async fn read_data<S>(&mut self, input: &mut S) -> Result<Vec<u8>, VncError>
    where
        S: AsyncRead + Unpin,
    {
        let len = read_compact_length(input).await?;
        ensure_payload_limit(
            "tight encoded payload",
            len,
            self.limits.max_encoded_payload_bytes,
        )?;
        let mut data = uninit_vec(len);
        input.read_exact(&mut data).await?;
        Ok(data)
    }

    async fn fill_rect<S, F, Fut>(
        &mut self,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        let mut color = [0; 3];
        input.read_exact(&mut color).await?;
        let image_size = checked_rgba_size(rect, &self.limits)?;
        let pixel_count = checked_pixel_count(rect)?;
        let mut image = Vec::with_capacity(image_size);

        let true_color = self.to_true_color(format, &color);

        for _ in 0..pixel_count {
            image.extend_from_slice(&true_color);
        }
        if image.len() != image_size {
            return Err(VncError::InvalidImageData);
        }
        output_func(VncEvent::RawImage(*rect, image)).await?;
        Ok(())
    }

    async fn jpeg_rect<S, F, Fut>(
        &mut self,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        let output_size = checked_rgba_size(rect, &self.limits)?;
        let data = self.read_data(input).await?;
        let mut reader = ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .map_err(|_| VncError::InvalidImageData)?;
        let mut image_limits = ImageLimits::default();
        image_limits.max_image_width = Some(u32::from(rect.width));
        image_limits.max_image_height = Some(u32::from(rect.height));
        image_limits.max_alloc = Some(output_size as u64);
        reader.limits(image_limits);
        let image = reader.decode().map_err(|_| VncError::InvalidImageData)?;
        if image.dimensions() != (u32::from(rect.width), u32::from(rect.height)) {
            return Err(VncError::InvalidImageData);
        }

        let mut rgba = Vec::with_capacity(output_size);
        for pixel in image.to_rgb8().pixels() {
            rgba.extend_from_slice(&self.to_true_color(format, &pixel.0));
        }
        if rgba.len() != output_size {
            return Err(VncError::InvalidImageData);
        }
        output_func(VncEvent::RawImage(*rect, rgba)).await?;
        Ok(())
    }

    async fn basic_rect<S, F, Fut>(
        &mut self,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        self.filter = {
            if self.ctrl & 0x4 == 4 {
                input.read_u8().await?
            } else {
                0
            }
        };

        let stream_id = self.ctrl & 0x3;
        match self.filter {
            0 => {
                // copy filter
                self.copy_filter(stream_id, format, rect, input, output_func)
                    .await
            }
            1 => {
                // palette
                self.palette_filter(stream_id, format, rect, input, output_func)
                    .await
            }
            2 => {
                // gradient
                self.gradient_filter(stream_id, format, rect, input, output_func)
                    .await
            }
            _ => {
                error!("Illegal tight filter received (filter: {})", self.filter);
                Err(VncError::InvalidImageData)
            }
        }
    }

    async fn copy_filter<S, F, Fut>(
        &mut self,
        stream: u8,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        let uncompressed_size = checked_rgb_size(rect, &self.limits)?;
        if uncompressed_size == 0 {
            return Ok(());
        };

        let data = self
            .read_tight_data(stream, input, uncompressed_size)
            .await?;
        let image_size = checked_rgba_size(rect, &self.limits)?;
        let mut image = Vec::with_capacity(image_size);
        let mut j = 0;
        while j < uncompressed_size {
            image.extend_from_slice(&self.to_true_color(format, &data[j..j + 3]));
            j += 3;
        }
        if image.len() != image_size {
            return Err(VncError::InvalidImageData);
        }

        output_func(VncEvent::RawImage(*rect, image)).await?;

        Ok(())
    }

    async fn palette_filter<S, F, Fut>(
        &mut self,
        stream: u8,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        let num_colors = input.read_u8().await? as usize + 1;
        let palette_size = num_colors
            .checked_mul(3)
            .ok_or(VncError::IntegerOverflow("tight palette size"))?;

        self.palette = uninit_vec(palette_size);
        input.read_exact(&mut self.palette).await?;

        let bpp = if num_colors <= 2 { 1 } else { 8 };
        let row_size = usize::from(rect.width)
            .checked_mul(bpp)
            .ok_or(VncError::IntegerOverflow("tight palette row bits"))?
            .div_ceil(8);
        let uncompressed_size = usize::from(rect.height)
            .checked_mul(row_size)
            .ok_or(VncError::IntegerOverflow("tight palette decoded bytes"))?;
        ensure_payload_limit(
            "tight palette decoded payload",
            uncompressed_size,
            self.limits.max_decoded_payload_bytes,
        )?;

        if uncompressed_size == 0 {
            return Ok(());
        }

        let data = self
            .read_tight_data(stream, input, uncompressed_size)
            .await?;

        if num_colors == 2 {
            self.mono_rect(data, rect, format, output_func).await?
        } else {
            self.palette_rect(data, rect, format, output_func).await?
        }

        Ok(())
    }

    async fn mono_rect<F, Fut>(
        &mut self,
        data: Vec<u8>,
        rect: &Rect,
        format: &PixelFormat,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        // Convert indexed (palette based) image data to RGB
        let total = checked_pixel_count(rect)?;
        let mut image = uninit_vec(checked_rgba_size(rect, &self.limits)?);
        let mut offset = 8_usize;
        let mut index = 0_usize;
        let mut dp = 0;
        for i in 0..total {
            if i % usize::from(rect.width) == 0 {
                offset = 8;
                if i != 0 {
                    index = index
                        .checked_add(1)
                        .ok_or(VncError::IntegerOverflow("tight mono row index"))?;
                }
            } else if offset == 0 {
                offset = 8;
                index = index
                    .checked_add(1)
                    .ok_or(VncError::IntegerOverflow("tight mono byte index"))?;
            }
            let packed = data.get(index).ok_or(VncError::InvalidImageData)?;
            offset -= 1;
            let sp = usize::from((packed >> offset) & 0x01) * 3;
            let true_color = self.to_true_color(format, &self.palette[sp..sp + 3]);
            image[dp..dp + 4].copy_from_slice(&true_color);
            dp += 4;
        }
        output_func(VncEvent::RawImage(*rect, image)).await?;
        Ok(())
    }

    async fn palette_rect<F, Fut>(
        &mut self,
        data: Vec<u8>,
        rect: &Rect,
        format: &PixelFormat,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        // Convert indexed (palette based) image data to RGB
        let total = checked_pixel_count(rect)?;
        let mut image = uninit_vec(checked_rgba_size(rect, &self.limits)?);
        let mut i = 0;
        let mut dp = 0;
        while i < total {
            let sp = data[i] as usize * 3;
            let color = self
                .palette
                .get(sp..sp + 3)
                .ok_or(VncError::InvalidImageData)?;
            let true_color = self.to_true_color(format, color);
            image[dp..dp + 4].copy_from_slice(&true_color);
            dp += 4;
            i += 1;
        }
        output_func(VncEvent::RawImage(*rect, image)).await?;
        Ok(())
    }

    async fn gradient_filter<S, F, Fut>(
        &mut self,
        stream: u8,
        format: &PixelFormat,
        rect: &Rect,
        input: &mut S,
        output_func: &F,
    ) -> Result<(), VncError>
    where
        S: AsyncRead + Unpin,
        F: Fn(VncEvent) -> Fut,
        Fut: Future<Output = Result<(), VncError>>,
    {
        let uncompressed_size = checked_rgb_size(rect, &self.limits)?;
        if uncompressed_size == 0 {
            return Ok(());
        };
        let data = self
            .read_tight_data(stream, input, uncompressed_size)
            .await?;
        let mut image = uninit_vec(checked_rgba_size(rect, &self.limits)?);

        let row_len = usize::from(rect.width)
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(3))
            .ok_or(VncError::IntegerOverflow("tight gradient row length"))?;
        let mut row_0 = vec![0_u16; row_len];
        let mut row_1 = vec![0_u16; row_len];
        let max = [format.red_max, format.green_max, format.blue_max];
        let mut sp = 0;
        let mut dp = 0;

        for y in 0..rect.height as usize {
            let (this_row, prev_row) = match y & 1 {
                0 => (&mut row_0, &mut row_1),
                1 => (&mut row_1, &mut row_0),
                _ => return Err(VncError::WrongPixelFormat),
            };
            let mut x = 3;
            while x < row_len {
                let rgb = &data[sp..sp + 3];
                for index in 0..3 {
                    let d = prev_row[index + x] as i32 + this_row[index + x - 3] as i32
                        - prev_row[index + x - 3] as i32;
                    let converted = if d < 0 {
                        0
                    } else if d > max[index] as i32 {
                        max[index]
                    } else {
                        d as u16
                    };
                    this_row[index + x] = (converted + rgb[index] as u16) & max[index];
                }
                let color = [
                    this_row[x] as u8,
                    this_row[x + 1] as u8,
                    this_row[x + 2] as u8,
                ];
                image[dp..dp + 4].copy_from_slice(&self.to_true_color(format, &color));
                dp += 4;
                sp += 3;
                x += 3;
            }
        }

        output_func(VncEvent::RawImage(*rect, image)).await?;
        Ok(())
    }

    async fn read_tight_data<S>(
        &mut self,
        stream: u8,
        input: &mut S,
        uncompressed_size: usize,
    ) -> Result<Vec<u8>, VncError>
    where
        S: AsyncRead + Unpin,
    {
        ensure_payload_limit(
            "tight decoded payload",
            uncompressed_size,
            self.limits.max_decoded_payload_bytes,
        )?;
        let mut data;
        if uncompressed_size < 12 {
            data = uninit_vec(uncompressed_size);
            input.read_exact(&mut data).await?;
        } else {
            let d = self.read_data(input).await?;
            let decompressor = self.zlibs[stream as usize]
                .take()
                .ok_or(VncError::InvalidImageData)?;
            let mut reader = ZlibReader::new(decompressor, &d);
            data = uninit_vec(uncompressed_size);
            reader.read_exact(&mut data)?;
            self.zlibs[stream as usize] = Some(reader.into_inner()?);
        };
        Ok(data)
    }

    fn to_true_color(&self, format: &PixelFormat, color: &[u8]) -> [u8; 4] {
        rgb_to_pixel(format, self.alpha_shift, color).expect("validated tight RGB color")
    }
}

async fn read_compact_length<S>(input: &mut S) -> Result<usize, VncError>
where
    S: AsyncRead + Unpin,
{
    let first = input.read_u8().await?;
    let mut len = usize::from(first & 0x7f);
    if first & 0x80 != 0 {
        let second = input.read_u8().await?;
        len |= usize::from(second & 0x7f) << 7;
        if second & 0x80 != 0 {
            let third = input.read_u8().await?;
            if third & 0x80 != 0 {
                return Err(VncError::InvalidImageData);
            }
            len |= usize::from(third) << 14;
        }
    }
    Ok(len)
}

