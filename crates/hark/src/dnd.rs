//! Handing files to other programs: a drag out of the window, and a file copy to the system
//! clipboard. winit 0.30 has neither, so each desktop gets its own piece behind the same two
//! calls; X11 has no drag source at all, and the status line says so. Windows and macOS drag
//! through the `drag` crate, except that a Windows drag of a file still being written has a
//! source of its own.
//!
//! A drag of a file still being written starts at once on every desktop, and its drop target
//! is answered when it reads the file's name, once the file is there.

use std::path::PathBuf;

use crate::promise::Promise;

#[cfg(windows)]
mod ole;
#[cfg(all(unix, not(target_os = "macos")))]
mod wayland;

/// The picture under the pointer during a drag: straight-alpha RGBA, `scale` buffer pixels
/// per logical one. Windows sizes the picture itself.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    #[cfg_attr(windows, allow(dead_code))]
    pub scale: i32,
}

/// Whether a dragged file has been written yet. The drag starts the moment the pointer moves,
/// with the path known; the file lands on a worker thread, and the drop target is answered
/// only once it has.
#[derive(Clone)]
pub struct Ready(Promise<bool>);

impl Ready {
    pub fn new() -> Ready {
        Ready(Promise::new())
    }

    pub fn set(&self, ok: bool) {
        self.0.set(ok);
    }

    /// Blocks until the file is there, or gives up after a couple of minutes.
    pub fn wait(&self) -> bool {
        self.0.wait(std::time::Duration::from_secs(120)) == Some(true)
    }
}

/// The window's drag source, if the desktop has one.
pub struct DragOut {
    #[cfg(all(unix, not(target_os = "macos")))]
    wayland: Option<wayland::Dragger>,
    #[cfg(any(windows, target_os = "macos"))]
    native: Option<raw_window_handle::RawWindowHandle>,
}

impl DragOut {
    pub fn new(cc: &eframe::CreationContext<'_>) -> DragOut {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let window = cc.window_handle().map(|h| h.as_raw());
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            let display = cc.display_handle().map(|h| h.as_raw());
            let wayland = match (display, window) {
                (Ok(RawDisplayHandle::Wayland(d)), Ok(RawWindowHandle::Wayland(w))) => {
                    // SAFETY: winit's live wl_display and the window's wl_surface; both outlive
                    // the app, which is created once for the process.
                    match unsafe { wayland::Dragger::new(d.display.as_ptr(), w.surface.as_ptr()) } {
                        Ok(d) => Some(d),
                        Err(e) => {
                            eprintln!("drag source: {e}");
                            None
                        }
                    }
                }
                _ => None,
            };
            DragOut { wayland }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let native = match window {
                Ok(h @ (RawWindowHandle::Win32(_) | RawWindowHandle::AppKit(_))) => Some(h),
                _ => None,
            };
            // winit registers every window for file drops on macOS; hark takes none, and as a
            // drop target the window would read the name of a file it is itself dragging out,
            // which waits for the file to be written.
            #[cfg(target_os = "macos")]
            if let Some(RawWindowHandle::AppKit(h)) = native {
                // SAFETY: the window's live NSView, on the main thread that created it.
                let view = unsafe { &*(h.ns_view.as_ptr() as *const objc2_app_kit::NSView) };
                if let Some(window) = view.window() {
                    window.unregisterDraggedTypes();
                }
            }
            DragOut { native }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = window;
            DragOut {}
        }
    }

    /// Starts dragging `paths` from the window, with `image` under the pointer, and with the
    /// drop answered only once `ready` says the file is written. The pointer button must still
    /// be down.
    pub fn start(&self, paths: Vec<PathBuf>, image: Option<Image>, ready: Option<Ready>) -> Result<(), String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            match &self.wayland {
                Some(d) => d.drag(paths, image, ready),
                None => Err("dragging out of the window needs Wayland; on X11, use copy".into()),
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let Some(handle) = self.native else { return Err("no window handle for the drag".into()) };
            #[cfg(windows)]
            if let Some(ready) = ready {
                return ole::drag(paths, image, ready);
            }
            #[cfg(windows)]
            let item = drag::DragItem::Files(paths);
            #[cfg(target_os = "macos")]
            let item = match ready {
                Some(ready) => promised(paths, ready),
                None => drag::DragItem::Files(paths),
            };
            struct Native(raw_window_handle::RawWindowHandle);
            impl raw_window_handle::HasWindowHandle for Native {
                fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
                    // SAFETY: the handle is the live window's, held for the app's lifetime.
                    Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.0) })
                }
            }
            drag::start_drag(&Native(handle), item, drag::Image::Raw(png(image)), |_, _| {}, drag::Options::default()).map_err(|e| e.to_string())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (paths, image, ready);
            Err("dragging out of the window is not available here; use copy".into())
        }
    }
}

/// A drag item for a file still being written: it declares the file's URL, and the legacy
/// filename list older programs read, and gives them only when the drop target reads one,
/// once the file is there. The target waits meanwhile, as a Wayland one does.
#[cfg(target_os = "macos")]
fn promised(paths: Vec<PathBuf>, ready: Ready) -> drag::DragItem {
    const FILE_URL: &str = "public.file-url";
    const FILENAMES: &str = "NSFilenamesPboardType";
    let provider = move |kind: &str| -> Option<Vec<u8>> {
        if !ready.wait() {
            return None;
        }
        match kind {
            FILE_URL => paths.first().map(|p| file_uri(p).into_bytes()),
            FILENAMES => {
                let escape = |s: String| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                let names: String = paths.iter().map(|p| format!("<string>{}</string>", escape(p.to_string_lossy().into_owned()))).collect();
                Some(format!(r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><array>{names}</array></plist>"#).into_bytes())
            }
            _ => None,
        }
    };
    drag::DragItem::Data { provider: Box::new(provider), types: vec![FILE_URL.into(), FILENAMES.into()] }
}

#[cfg(windows)]
fn png(image: Option<Image>) -> Vec<u8> {
    let Some(im) = image else { return Vec::new() };
    let mut png = std::io::Cursor::new(Vec::new());
    match image::write_buffer_with_format(&mut png, &im.rgba, im.width, im.height, image::ColorType::Rgba8, image::ImageFormat::Png) {
        Ok(()) => png.into_inner(),
        Err(_) => Vec::new(),
    }
}

/// AppKit sizes the picture from the PNG's resolution, 72 dpi to the point, and `drag` panics
/// on data it cannot decode, so a drag without a picture carries a transparent pixel.
#[cfg(target_os = "macos")]
fn png(image: Option<Image>) -> Vec<u8> {
    let im = image.unwrap_or(Image { width: 1, height: 1, rgba: vec![0; 4], scale: 1 });
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, im.width, im.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let per_metre = (f64::from(im.scale) * 72.0 / 0.0254).round() as u32;
    enc.set_pixel_dims(Some(png::PixelDimensions { xppu: per_metre, yppu: per_metre, unit: png::Unit::Meter }));
    enc.write_header().and_then(|mut w| w.write_image_data(&im.rgba)).expect("an RGBA buffer of its stated size encodes");
    out
}

/// Puts `paths` on the clipboard as files, the way a file manager's copy does.
pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};
        let uris = uri_list(paths);
        let gnome = format!("copy\n{}", uris.trim_end_matches('\n').replace("\r\n", "\n"));
        let sources = vec![
            MimeSource { source: Source::Bytes(uris.clone().into_bytes().into()), mime_type: MimeType::Specific("text/uri-list".into()) },
            MimeSource { source: Source::Bytes(gnome.into_bytes().into()), mime_type: MimeType::Specific("x-special/gnome-copied-files".into()) },
            MimeSource { source: Source::Bytes(uris.into_bytes().into()), mime_type: MimeType::Text },
        ];
        Options::new().copy_multi(sources).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let _open = clipboard_win::Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        clipboard_win::Setter::write_clipboard(&clipboard_win::formats::FileList, &list[..]).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::runtime::ProtocolObject;
        use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
        use objc2_foundation::{NSArray, NSString, NSURL};
        let urls: Vec<_> = paths.iter().map(|p| ProtocolObject::<dyn NSPasteboardWriting>::from_retained(NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))).collect();
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        if board.writeObjects(&NSArray::from_retained_slice(&urls)) {
            Ok(())
        } else {
            Err("the clipboard did not take the files".into())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = paths;
        Err("copying files to the clipboard is not available here".into())
    }
}

/// `text/uri-list`: one `file://` URI per line, CRLF-terminated, with the path percent-encoded.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| format!("{}\r\n", file_uri(p))).collect()
}

#[cfg(unix)]
fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    let bytes = path.as_os_str().as_bytes();
    if !bytes.starts_with(b"/") {
        out.push('/');
    }
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
