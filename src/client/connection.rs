use futures::TryStreamExt;
use tokio_stream::wrappers::ReceiverStream;

use std::{
    future::Future,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{
        mpsc::{channel, error::TryRecvError, Receiver, Sender},
        oneshot, Mutex,
    },
};
use tokio_util::compat::*;
use tracing::*;

use crate::{codec, PixelFormat, Rect, VncEncoding, VncError, VncEvent, VncLimits, X11Event};
const NETWORK_CHUNK_SIZE: usize = 64 * 1024;

mod output;

#[cfg(not(target_arch = "wasm32"))]
use tokio::spawn;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local as spawn;

use super::messages::{ClientMsg, ServerMsg};
use super::resize::DesktopState;

struct ImageRect {
    rect: Rect,
    encoding: VncEncoding,
}

impl TryFrom<[u8; 12]> for ImageRect {
    type Error = VncError;

    fn try_from(buf: [u8; 12]) -> Result<Self, Self::Error> {
        Ok(Self {
            rect: Rect {
                x: u16::from_be_bytes([buf[0], buf[1]]),
                y: u16::from_be_bytes([buf[2], buf[3]]),
                width: u16::from_be_bytes([buf[4], buf[5]]),
                height: u16::from_be_bytes([buf[6], buf[7]]),
            },
            encoding: VncEncoding::try_from(u32::from_be_bytes([
                buf[8], buf[9], buf[10], buf[11],
            ]))?,
        })
    }
}

impl ImageRect {
    async fn read<S>(reader: &mut S) -> Result<Self, VncError>
    where
        S: AsyncRead + Unpin,
    {
        let mut rect_buf = [0_u8; 12];
        reader.read_exact(&mut rect_buf).await?;
        rect_buf.try_into()
    }
}

/// The framebuffer size is shared between the decoder, which learns of resizes,
/// and the input path, which sizes refresh requests; pack it into one atomic.
fn pack_screen((width, height): (u16, u16)) -> u32 {
    (u32::from(width) << 16) | u32::from(height)
}

fn unpack_screen(packed: u32) -> (u16, u16) {
    ((packed >> 16) as u16, packed as u16)
}

struct VncInner {
    name: String,
    screen: Arc<AtomicU32>,
    desktop: Arc<DesktopState>,
    input_ch: Sender<ClientMsg>,
    output_ch: Receiver<VncEvent>,
    decoding_stop: Option<oneshot::Sender<()>>,
    net_conn_stop: Option<oneshot::Sender<()>>,
    closed: bool,
    limits: VncLimits,
}

/// The instance of a connected vnc client
///
impl VncInner {
    async fn new<S>(
        mut stream: S,
        shared: bool,
        mut pixel_format: Option<PixelFormat>,
        encodings: Vec<VncEncoding>,
        limits: VncLimits,
    ) -> Result<Self, VncError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (conn_ch_tx, conn_ch_rx) = channel(limits.channel_capacity);
        let (input_ch_tx, input_ch_rx) = channel(limits.channel_capacity);
        let (output_ch_tx, output_ch_rx) = channel(limits.channel_capacity);
        let (decoding_stop_tx, mut decoding_stop_rx) = oneshot::channel();
        let (net_conn_stop_tx, net_conn_stop_rx) = oneshot::channel();

        trace!("client init msg");
        send_client_init(&mut stream, shared).await?;

        trace!("server init msg");
        let (name, (width, height)) =
            read_server_init(&mut stream, &mut pixel_format, &limits, &|e| async {
                output_ch_tx.send(e).await?;
                Ok(())
            })
            .await?;

        let screen = Arc::new(AtomicU32::new(pack_screen((width, height))));
        let decoder_screen = Arc::clone(&screen);
        let desktop = Arc::new(DesktopState::default());
        let decoder_desktop = Arc::clone(&desktop);
        let network_desktop = Arc::clone(&desktop);
        trace!("client encodings: {:?}", encodings);
        send_client_encoding(&mut stream, encodings.clone()).await?;

        trace!("Require the first frame");
        input_ch_tx
            .send(ClientMsg::FramebufferUpdateRequest(
                Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                0,
            ))
            .await?;

        // start the decoding thread
        spawn(async move {
            trace!("Decoding thread starts");
            let mut conn_ch_rx = {
                let conn_ch_rx = ReceiverStream::new(conn_ch_rx).into_async_read();
                FuturesAsyncReadCompatExt::compat(conn_ch_rx)
            };

            let output_func = |e| async {
                output_ch_tx.send(e).await?;
                Ok(())
            };

            let Some(pf) = pixel_format.as_ref() else {
                decoder_desktop.close();
                output::report_error(VncError::WrongPixelFormat, &output_ch_tx, &mut decoding_stop_rx).await;
                return;
            };
            let result = asycn_vnc_read_loop(
                &mut conn_ch_rx,
                pf,
                &limits,
                &output_func,
                &mut decoding_stop_rx,
                &encodings,
                &decoder_screen,
                &decoder_desktop,
            )
            .await;
            // Release the network worker and fail any pending resize before waiting
            // for output capacity.
            drop(conn_ch_rx);
            decoder_desktop.close();
            if let Err(error) = result {
                output::report_error(error, &output_ch_tx, &mut decoding_stop_rx).await;
            }
            trace!("Decoding thread stops");
        });

        // start the traffic process thread
        spawn(async move {
            trace!("Net Connection thread starts");
            let _ =
                async_connection_process_loop(stream, input_ch_rx, conn_ch_tx, net_conn_stop_rx)
                    .await;
            network_desktop.close();
            trace!("Net Connection thread stops");
        });

        info!("VNC Client {name} starts");
        Ok(Self {
            name,
            screen,
            desktop,
            input_ch: input_ch_tx,
            output_ch: output_ch_rx,
            decoding_stop: Some(decoding_stop_tx),
            net_conn_stop: Some(net_conn_stop_tx),
            closed: false,
            limits,
        })
    }

    fn input_message(&self, event: X11Event) -> Result<ClientMsg, VncError> {
        if self.closed {
            Err(VncError::ClientNotRunning)
        } else {
            let (width, height) = unpack_screen(self.screen.load(Ordering::Acquire));
            let msg = match event {
                X11Event::Refresh => ClientMsg::FramebufferUpdateRequest(
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    1,
                ),
                X11Event::FullRefresh => ClientMsg::FramebufferUpdateRequest(
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    0, // non-incremental: server sends entire framebuffer
                ),
                X11Event::KeyEvent(key) => ClientMsg::KeyEvent(key.keycode, key.down),
                X11Event::PointerEvent(mouse) => {
                    ClientMsg::PointerEvent(mouse.position_x, mouse.position_y, mouse.bottons)
                }
                X11Event::CopyText(text) => {
                    ensure_limit(
                        "client clipboard",
                        text.len(),
                        self.limits.max_clipboard_bytes,
                    )?;
                    ClientMsg::ClientCutText(text)
                }
            };
            Ok(msg)
        }
    }

    async fn recv_event(&mut self) -> Result<VncEvent, VncError> {
        if self.closed {
            Err(VncError::ClientNotRunning)
        } else {
            match self.output_ch.recv().await {
                Some(e) => Ok(e),
                None => {
                    self.closed = true;
                    Err(VncError::ClientNotRunning)
                }
            }
        }
    }

    async fn poll_event(&mut self) -> Result<Option<VncEvent>, VncError> {
        if self.closed {
            Err(VncError::ClientNotRunning)
        } else {
            match self.output_ch.try_recv() {
                Err(TryRecvError::Disconnected) => {
                    self.closed = true;
                    Err(VncError::ClientNotRunning)
                }
                Err(TryRecvError::Empty) => Ok(None),
                Ok(e) => Ok(Some(e)),
            }
            // Ok(self.output_ch.recv().await)
        }
    }

    /// Stop the VNC engine and release resources
    ///
    fn close(&mut self) -> Result<(), VncError> {
        self.desktop.close();
        if let Some(net_conn_stop) = self.net_conn_stop.take() {
            let _ = net_conn_stop.send(());
        }
        if let Some(decoding_stop) = self.decoding_stop.take() {
            let _ = decoding_stop.send(());
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for VncInner {
    fn drop(&mut self) {
        info!("VNC Client {} stops", self.name);
        let _ = self.close();
    }
}

pub struct VncClient {
    server_name: Arc<str>,
    inner: Arc<Mutex<VncInner>>,
    desktop: Arc<DesktopState>,
    input_ch: Sender<ClientMsg>,
}

impl VncClient {
    pub(super) async fn new<S>(
        stream: S,
        shared: bool,
        pixel_format: Option<PixelFormat>,
        encodings: Vec<VncEncoding>,
        limits: VncLimits,
    ) -> Result<Self, VncError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let inner = VncInner::new(stream, shared, pixel_format, encodings, limits).await?;
        Ok(Self {
            server_name: Arc::from(inner.name.as_str()),
            desktop: Arc::clone(&inner.desktop),
            input_ch: inner.input_ch.clone(),
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    /// Desktop name advertised during the initial server handshake.
    /// This is a server-supplied label, not an authenticated machine identity.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Last validated ExtendedDesktopSize layout; None means support is unconfirmed.
    pub fn desktop_layout(&self) -> Option<crate::DesktopLayout> {
        self.desktop.layout()
    }

    /// Request a negotiated single-screen resize and wait for the server result.
    /// Available on native targets with a Tokio time driver; wasm supports observation only.
    /// Continue draining events concurrently. Each queue/confirmation wait is bounded
    /// to five seconds. Cancellation after dispatch makes later requests uncertain;
    /// reconnect and observe before retrying. Dispatch includes one incremental one-pixel
    /// update request so servers can deliver the confirmation.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resize_desktop(
        &self,
        width: u16,
        height: u16,
    ) -> Result<crate::DesktopLayout, crate::ResizeError> {
        self.desktop.request(&self.input_ch, width, height).await
    }

    /// Input a `X11Event` from the frontend
    ///
    pub async fn input(&self, event: X11Event) -> Result<(), VncError> {
        let sender = {
            let inner = self.inner.lock().await;
            if inner.closed {
                return Err(VncError::ClientNotRunning);
            }
            inner.input_ch.clone()
        };
        // Do not hold the client mutex while backpressure waits: close needs it.
        let permit = sender.reserve().await?;
        let inner = self.inner.lock().await;
        permit.send(inner.input_message(event)?);
        Ok(())
    }

    /// Receive a `VncEvent` from the engine
    /// This function will block until a `VncEvent` is received
    ///
    pub async fn recv_event(&self) -> Result<VncEvent, VncError> {
        self.inner.lock().await.recv_event().await
    }

    /// polling `VncEvent` from the engine and give it to the client
    ///
    pub async fn poll_event(&self) -> Result<Option<VncEvent>, VncError> {
        self.inner.lock().await.poll_event().await
    }

    /// Stop the VNC engine and release resources
    ///
    pub async fn close(&self) -> Result<(), VncError> {
        self.inner.lock().await.close()
    }
}

impl Clone for VncClient {
    fn clone(&self) -> Self {
        Self {
            server_name: Arc::clone(&self.server_name),
            inner: self.inner.clone(),
            desktop: Arc::clone(&self.desktop),
            input_ch: self.input_ch.clone(),
        }
    }
}

fn ensure_limit(field: &'static str, actual: usize, limit: usize) -> Result<(), VncError> {
    if actual > limit {
        return Err(VncError::LimitExceeded {
            field,
            actual: actual as u64,
            limit: limit as u64,
        });
    }
    Ok(())
}

fn validate_framebuffer(width: u16, height: u16, limits: &VncLimits) -> Result<(), VncError> {
    if width == 0
        || height == 0
        || width > limits.max_framebuffer_width
        || height > limits.max_framebuffer_height
    {
        return Err(VncError::InvalidDimensions);
    }
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or(VncError::IntegerOverflow("framebuffer pixels"))?;
    ensure_limit("framebuffer pixels", pixels, limits.max_framebuffer_pixels)
}

fn validate_rect(rect: &Rect, limits: &VncLimits) -> Result<(), VncError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(VncError::InvalidDimensions);
    }
    rect.x
        .checked_add(rect.width)
        .ok_or(VncError::InvalidDimensions)?;
    rect.y
        .checked_add(rect.height)
        .ok_or(VncError::InvalidDimensions)?;
    let pixels = usize::from(rect.width)
        .checked_mul(usize::from(rect.height))
        .ok_or(VncError::IntegerOverflow("rectangle pixels"))?;
    ensure_limit("rectangle pixels", pixels, limits.max_framebuffer_pixels)
}

async fn send_client_init<S>(stream: &mut S, shared: bool) -> Result<(), VncError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    trace!("Send shared flag: {}", shared);
    stream.write_u8(shared as u8).await?;
    Ok(())
}

async fn read_server_init<S, F, Fut>(
    stream: &mut S,
    pf: &mut Option<PixelFormat>,
    limits: &VncLimits,
    output_func: &F,
) -> Result<(String, (u16, u16)), VncError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(VncEvent) -> Fut,
    Fut: Future<Output = Result<(), VncError>>,
{
    // +--------------+--------------+------------------------------+
    // | No. of bytes | Type [Value] | Description                  |
    // +--------------+--------------+------------------------------+
    // | 2            | U16          | framebuffer-width in pixels  |
    // | 2            | U16          | framebuffer-height in pixels |
    // | 16           | PIXEL_FORMAT | server-pixel-format          |
    // | 4            | U32          | name-length                  |
    // | name-length  | U8 array     | name-string                  |
    // +--------------+--------------+------------------------------+

    let screen_width = stream.read_u16().await?;
    let screen_height = stream.read_u16().await?;
    validate_framebuffer(screen_width, screen_height, limits)?;
    let mut send_our_pf = false;

    output_func(VncEvent::SetResolution(
        (screen_width, screen_height).into(),
    ))
    .await?;

    let pixel_format = PixelFormat::read(stream).await?;
    if pf.is_none() {
        output_func(VncEvent::SetPixelFormat(pixel_format)).await?;
        let _ = pf.insert(pixel_format);
    } else {
        send_our_pf = true;
    }

    let name_len = stream.read_u32().await? as usize;
    ensure_limit("server name", name_len, limits.max_server_name_bytes)?;
    let mut name_buf = vec![0_u8; name_len];
    stream.read_exact(&mut name_buf).await?;
    let name = String::from_utf8_lossy(&name_buf).into_owned();

    if send_our_pf {
        trace!("Send customized pixel format {:#?}", pf);
        let selected = pf.as_ref().copied().ok_or(VncError::WrongPixelFormat)?;
        ClientMsg::SetPixelFormat(selected).write(stream).await?;
    }
    Ok((name, (screen_width, screen_height)))
}

async fn send_client_encoding<S>(
    stream: &mut S,
    encodings: Vec<VncEncoding>,
) -> Result<(), VncError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    ClientMsg::SetEncodings(encodings).write(stream).await?;
    Ok(())
}

async fn asycn_vnc_read_loop<S, F, Fut>(
    stream: &mut S,
    pf: &PixelFormat,
    limits: &VncLimits,
    output_func: &F,
    stop_ch: &mut oneshot::Receiver<()>,
    encodings: &[VncEncoding],
    screen: &AtomicU32,
    desktop: &DesktopState,
) -> Result<(), VncError>
where
    S: AsyncRead + Unpin,
    F: Fn(VncEvent) -> Fut,
    Fut: Future<Output = Result<(), VncError>>,
{
    tokio::select! {
        biased;
        _ = stop_ch => Ok(()),
        result = read_vnc_messages(stream, pf, limits, output_func, encodings, screen, desktop) => result,
    }
}

async fn read_vnc_messages<S, F, Fut>(
    stream: &mut S,
    pf: &PixelFormat,
    limits: &VncLimits,
    output_func: &F,
    encodings: &[VncEncoding],
    shared_screen: &AtomicU32,
    desktop: &DesktopState,
) -> Result<(), VncError>
where
    S: AsyncRead + Unpin,
    F: Fn(VncEvent) -> Fut,
    Fut: Future<Output = Result<(), VncError>>,
{
    let mut raw_decoder = codec::RawDecoder::new(*limits);
    let mut zrle_decoder = codec::ZrleDecoder::new(*limits);
    let mut tight_decoder = codec::TightDecoder::new(*limits);
    let mut trle_decoder = codec::TrleDecoder::new(*limits);
    let mut cursor = codec::CursorDecoder::new(*limits);
    let mut screen = unpack_screen(shared_screen.load(Ordering::Acquire));

    // main decoding loop
    loop {
        let server_msg = ServerMsg::read(stream, limits).await?;
        trace!("Server message got: {:?}", server_msg);
        match server_msg {
            ServerMsg::FramebufferUpdate(rect_num) => {
                ensure_limit("rectangles per update", usize::from(rect_num), usize::from(limits.max_rectangles_per_update))?;
                let mut updates = crate::desktop::UpdateBatch::default();
                let mut framebuffer_changed = false;
                for _ in 0..rect_num {
                    let rect = ImageRect::read(stream).await?;
                    if rect.encoding != VncEncoding::Raw && !encodings.contains(&rect.encoding) {
                        return Err(VncError::InvalidImageData);
                    }
                    if !matches!(
                        rect.encoding,
                        VncEncoding::DesktopSizePseudo
                            | VncEncoding::ExtendedDesktopSizePseudo
                            | VncEncoding::LastRectPseudo
                            | VncEncoding::CursorPseudo
                    ) {
                        validate_rect(&rect.rect, limits)?;
                        crate::limits::rectangle(&rect.rect, screen)?;
                    }
                    if !matches!(
                        rect.encoding,
                        VncEncoding::CursorPseudo
                            | VncEncoding::ExtendedDesktopSizePseudo
                            | VncEncoding::LastRectPseudo
                    ) {
                        if !updates.is_empty() {
                            return Err(VncError::InvalidImageData);
                        }
                        framebuffer_changed = true;
                    }

                    match rect.encoding {
                        VncEncoding::Raw => {
                            raw_decoder
                                .decode(pf, &rect.rect, stream, output_func)
                                .await?;
                        }
                        VncEncoding::CopyRect => {
                            let source_x = stream.read_u16().await?;
                            let source_y = stream.read_u16().await?;
                            let mut src_rect = rect.rect;
                            src_rect.x = source_x;
                            src_rect.y = source_y;
                            crate::limits::rectangle(&src_rect, screen)?;
                            output_func(VncEvent::Copy(rect.rect, src_rect)).await?;
                        }
                        VncEncoding::Tight => {
                            tight_decoder
                                .decode(pf, &rect.rect, stream, output_func)
                                .await?;
                        }
                        VncEncoding::Trle => {
                            trle_decoder
                                .decode(pf, &rect.rect, stream, output_func)
                                .await?;
                        }
                        VncEncoding::Zrle => {
                            zrle_decoder
                                .decode(pf, &rect.rect, stream, output_func)
                                .await?;
                        }
                        VncEncoding::CursorPseudo => {
                            cursor.decode(pf, &rect.rect, stream, output_func).await?;
                        }
                        VncEncoding::ExtendedDesktopSizePseudo => {
                            // Confirm only after the entire message excludes framebuffer changes.
                            // Cursor metadata may accompany desktop layout updates.
                            if framebuffer_changed {
                                return Err(VncError::InvalidImageData);
                            }
                            updates.push(crate::DesktopUpdate::read(stream, rect.rect).await?)?;
                        }
                        VncEncoding::DesktopSizePseudo => {
                            crate::limits::dimensions(rect.rect.width, rect.rect.height)?;
                            if rect.rect.x != 0 || rect.rect.y != 0 {
                                return Err(VncError::InvalidImageData);
                            }
                            screen = (rect.rect.width, rect.rect.height);
                            shared_screen.store(pack_screen(screen), Ordering::Release);
                            desktop.legacy_resize();
                            output_func(VncEvent::SetResolution(
                                (rect.rect.width, rect.rect.height).into(),
                            ))
                            .await?;
                        }
                        VncEncoding::LastRectPseudo => {
                            break;
                        }
                    }
                }
                for update in updates.into_updates() {
                    if let Some(layout) = &update.layout {
                        screen = (layout.width, layout.height);
                        shared_screen.store(pack_screen(screen), Ordering::Release);
                    }
                    desktop.observe(&update);
                    output_func(VncEvent::DesktopUpdate(update)).await?;
                }
            }
            // SetColorMapEntries,
            ServerMsg::Bell => {
                output_func(VncEvent::Bell).await?;
            }
            ServerMsg::ServerCutText(text) => {
                output_func(VncEvent::Text(text)).await?;
            }
        }
    }
}

async fn async_connection_process_loop<S>(
    mut stream: S,
    mut input_ch: Receiver<ClientMsg>,
    conn_ch: Sender<std::io::Result<Vec<u8>>>,
    mut stop_ch: oneshot::Receiver<()>,
) -> Result<(), VncError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut buffer = [0; NETWORK_CHUNK_SIZE];
    let mut pending = 0;

    loop {
        tokio::select! {
            _ = &mut stop_ch => break,
            _ = conn_ch.closed() => break,
            permit = conn_ch.reserve(), if pending > 0 => {
                match permit {
                    Ok(permit) => {
                        permit.send(Ok(buffer[..pending].to_vec()));
                        pending = 0;
                    }
                    Err(_) => break,
                }
            }
            result = stream.read(&mut buffer), if pending == 0 => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(length) => pending = length,
                }
            }
            message = input_ch.recv() => {
                let Some(message) = message else { break; };
                tokio::select! {
                    biased;
                    _ = &mut stop_ch => break,
                    result = message.write(&mut stream) => result?,
                }
            }
        }
    }
    // Dropping the bridge signals EOF without blocking shutdown on a full queue.
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod queue_tests;

#[cfg(test)]
mod resize_tests;

#[cfg(test)]
mod fork_tests {
    use super::*;

    #[test]
    fn rejects_unknown_encoding_and_coordinate_overflow() {
        let mut wire = [0_u8; 12];
        wire[4..6].copy_from_slice(&1_u16.to_be_bytes());
        wire[6..8].copy_from_slice(&1_u16.to_be_bytes());
        wire[8..12].copy_from_slice(&99_i32.to_be_bytes());
        assert!(matches!(
            ImageRect::try_from(wire),
            Err(VncError::InvalidEncoding(99))
        ));

        let limits = VncLimits::default();
        let overflowing = Rect {
            x: u16::MAX,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(matches!(
            validate_rect(&overflowing, &limits),
            Err(VncError::InvalidDimensions)
        ));
    }

    #[test]
    fn rejects_oversized_framebuffer_and_pixel_product() {
        let limits = VncLimits {
            max_framebuffer_width: 100,
            max_framebuffer_height: 100,
            max_framebuffer_pixels: 50,
            ..VncLimits::default()
        };
        assert!(validate_framebuffer(101, 1, &limits).is_err());
        assert!(matches!(
            validate_framebuffer(10, 10, &limits),
            Err(VncError::LimitExceeded { .. })
        ));
    }
}
