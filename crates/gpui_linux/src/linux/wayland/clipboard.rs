use std::{
    io::{ErrorKind, Write},
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd},
};

use calloop::{LoopHandle, PostAction};
use filedescriptor::{FileDescriptor, Pipe};
use strum::IntoEnumIterator;
use wayland_client::{Connection, protocol::wl_data_offer::WlDataOffer};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1;

use crate::linux::platform::{PIPE_READ_TIMEOUT, read_fd_with_timeout};
use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat, hash};

/// Text mime types that we'll offer to other programs.
pub(crate) const TEXT_MIME_TYPES: [&str; 3] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
pub(crate) const FILE_LIST_MIME_TYPE: &str = "text/uri-list";

/// Text mime types that we'll accept from other programs.
pub(crate) const ALLOWED_TEXT_MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "UTF8_STRING"];

pub(crate) struct Clipboard {
    connection: Connection,
    loop_handle: LoopHandle<'static, ()>,
    self_mime: String,

    // Internal clipboard
    contents: Option<ClipboardItem>,
    primary_contents: Option<ClipboardItem>,

    // External clipboard
    cached_read: Option<ClipboardItem>,
    current_offer: Option<DataOffer<WlDataOffer>>,
    cached_primary_read: Option<ClipboardItem>,
    current_primary_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>,
}

pub(crate) trait ReceiveData {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>);
}

impl ReceiveData for WlDataOffer {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

impl ReceiveData for ZwpPrimarySelectionOfferV1 {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

#[derive(Clone, Debug)]
/// Wrapper for `WlDataOffer` and `ZwpPrimarySelectionOfferV1`, used to help track mime types.
pub(crate) struct DataOffer<T: ReceiveData> {
    pub inner: T,
    mime_types: Vec<String>,
}

impl<T: ReceiveData> DataOffer<T> {
    pub fn new(offer: T) -> Self {
        Self {
            inner: offer,
            mime_types: Vec::new(),
        }
    }

    pub fn add_mime_type(&mut self, mime_type: String) {
        self.mime_types.push(mime_type)
    }

    fn has_mime_type(&self, mime_type: &str) -> bool {
        self.mime_types.iter().any(|t| t == mime_type)
    }

    fn read_bytes(&self, connection: &Connection, mime_type: &str) -> Option<Vec<u8>> {
        let pipe = Pipe::new().unwrap();
        self.inner.receive_data(mime_type.to_string(), unsafe {
            BorrowedFd::borrow_raw(pipe.write.as_raw_fd())
        });
        let fd = pipe.read;
        drop(pipe.write);

        connection.flush().unwrap();

        match read_fd_with_timeout(fd, PIPE_READ_TIMEOUT) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    fn read_text(&self, connection: &Connection) -> Option<ClipboardItem> {
        let mime_type = self.mime_types.iter().find(|&mime_type| {
            ALLOWED_TEXT_MIME_TYPES
                .iter()
                .any(|&allowed| allowed == mime_type)
        })?;
        let bytes = self.read_bytes(connection, mime_type)?;
        let text_content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(e) => {
                log::error!("Failed to convert clipboard content to UTF-8: {}", e);
                return None;
            }
        };

        // Normalize the text to unix line endings, otherwise
        // copying from eg: firefox inserts a lot of blank
        // lines, and that is super annoying.
        let result = text_content.replace("\r\n", "\n");
        Some(ClipboardItem::new_string(result))
    }

    fn read_image(&self, connection: &Connection) -> Option<ClipboardItem> {
        for format in ImageFormat::iter() {
            let mime_type = format.mime_type();
            if !self.has_mime_type(mime_type) {
                continue;
            }

            if let Some(bytes) = self.read_bytes(connection, mime_type) {
                let id = hash(&bytes);
                return Some(ClipboardItem {
                    entries: vec![ClipboardEntry::Image(Image { format, bytes, id })],
                });
            }
        }
        None
    }
}

impl Clipboard {
    pub fn new(connection: Connection, loop_handle: LoopHandle<'static, ()>) -> Self {
        Self {
            connection,
            loop_handle,
            self_mime: format!("pid/{}", std::process::id()),

            contents: None,
            primary_contents: None,

            cached_read: None,
            current_offer: None,
            cached_primary_read: None,
            current_primary_offer: None,
        }
    }

    pub fn set(&mut self, item: ClipboardItem) {
        self.contents = Some(item);
    }

    pub fn set_primary(&mut self, item: ClipboardItem) {
        self.primary_contents = Some(item);
    }

    pub fn set_offer(&mut self, data_offer: Option<DataOffer<WlDataOffer>>) {
        self.cached_read = None;
        self.current_offer = data_offer;
    }

    pub fn set_primary_offer(&mut self, data_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>) {
        self.cached_primary_read = None;
        self.current_primary_offer = data_offer;
    }

    pub fn self_mime(&self) -> String {
        self.self_mime.clone()
    }

    pub fn send(&self, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self.contents.as_ref().and_then(|contents| contents.text()) {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn send_primary(&self, _mime_type: String, fd: OwnedFd) {
        if let Some(text) = self
            .primary_contents
            .as_ref()
            .and_then(|contents| contents.text())
        {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn read(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_offer.as_ref()?;
        if let Some(cached) = self.cached_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_read = Some(item.clone());
        Some(item)
    }

    pub fn read_primary(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_primary_offer.as_ref()?;
        if let Some(cached) = self.cached_primary_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.primary_contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_primary_read = Some(item.clone());
        Some(item)
    }

    pub fn send_bytes(&self, fd: OwnedFd, bytes: Vec<u8>) {
        send_bytes(&self.loop_handle, fd, bytes);
    }
}

fn send_bytes(loop_handle: &LoopHandle<'static, ()>, fd: OwnedFd, bytes: Vec<u8>) {
    // Take ownership without duplicating the descriptor. Readiness alone does not
    // make a large write nonblocking when the receiving application stops reading.
    let mut file = unsafe { FileDescriptor::from_raw_fd(fd.into_raw_fd()) };
    if let Err(err) = file.set_non_blocking(true) {
        log::error!("failed to make clipboard pipe nonblocking: {err}");
        return;
    }
    let mut written = 0;
    loop_handle
        .insert_source(
            calloop::generic::Generic::new(file, calloop::Interest::WRITE, calloop::Mode::Level),
            move |_, file, _| {
                let file = unsafe { file.get_mut() };
                Ok(write_available(file, &bytes, &mut written))
            },
        )
        .unwrap();
}

fn write_available(file: &mut impl Write, bytes: &[u8], written: &mut usize) -> PostAction {
    while *written < bytes.len() {
        match file.write(&bytes[*written..]) {
            Ok(0) => return PostAction::Remove,
            Ok(n) => *written += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
            Err(_) => return PostAction::Remove,
        }
    }
    PostAction::Remove
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        io::{self, Read},
        time::Duration,
    };

    struct ScriptedWriter {
        results: VecDeque<io::Result<usize>>,
        received: Vec<u8>,
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let result = self.results.pop_front().expect("unexpected extra write");
            if let Ok(n) = result {
                self.received.extend_from_slice(&bytes[..n]);
            }
            result
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn partial_writes_resume_after_backpressure_without_losing_bytes() {
        let mut writer = ScriptedWriter {
            results: VecDeque::from([
                Ok(2),
                Err(io::Error::from(ErrorKind::Interrupted)),
                Err(io::Error::from(ErrorKind::WouldBlock)),
                Ok(3),
            ]),
            received: Vec::new(),
        };
        let mut written = 0;
        assert!(matches!(
            write_available(&mut writer, b"abcde", &mut written),
            PostAction::Continue
        ));
        assert_eq!(written, 2);
        assert!(matches!(
            write_available(&mut writer, b"abcde", &mut written),
            PostAction::Remove
        ));
        assert_eq!(written, 5);
        assert_eq!(writer.received, b"abcde");
    }

    #[test]
    fn zero_write_removes_the_source_instead_of_spinning() {
        let mut writer = ScriptedWriter {
            results: VecDeque::from([Ok(0)]),
            received: Vec::new(),
        };
        assert!(matches!(
            write_available(&mut writer, b"data", &mut 0),
            PostAction::Remove
        ));
        assert!(writer.results.is_empty());
    }

    #[test]
    fn large_clipboard_transfer_yields_to_a_slow_pipe_reader() {
        let mut event_loop = calloop::EventLoop::<()>::try_new().unwrap();
        let mut pipe = Pipe::new().unwrap();
        pipe.read.set_non_blocking(true).unwrap();
        let raw_fd = pipe.write.as_raw_fd();
        let fd = unsafe { OwnedFd::from_raw_fd(pipe.write.into_raw_fd()) };
        let bytes = (0..256 * 1024).map(|n| n as u8).collect::<Vec<_>>();
        send_bytes(&event_loop.handle(), fd, bytes.clone());
        // Check before dispatch so a regression fails instead of hanging the test.
        let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::O_NONBLOCK, 0);

        // With no reader yet, this must return after filling the pipe.
        event_loop.dispatch(Duration::ZERO, &mut ()).unwrap();
        let mut received = Vec::new();
        let mut chunk = [0; 4096];
        for _ in 0..4096 {
            event_loop.dispatch(Duration::ZERO, &mut ()).unwrap();
            match pipe.read.read(&mut chunk) {
                Ok(0) => {
                    assert_eq!(received, bytes);
                    return;
                }
                Ok(n) => received.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == ErrorKind::WouldBlock => {}
                Err(err) => panic!("clipboard pipe read failed: {err}"),
            }
        }
        panic!("clipboard transfer did not finish");
    }
}
