//! Handing files to other programs: a drag out of the window, and a file copy to the system
//! clipboard. winit 0.30 has neither, so each desktop gets its own piece behind the same
//! calls; X11 has no drag source at all, and the status line says so. Windows and macOS drag
//! through the `drag` crate.
//!
//! A drag carries a file that is already written. A drop target may read the file's name at
//! any moment from the drag's start, and a late answer hangs a Windows or macOS target and
//! confuses a Wayland one, so the window holds a drag back until its file is there.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

/// Raised when a drag ends without a drop, from whichever thread learns it.
#[derive(Clone)]
struct Missed {
    flag: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl Missed {
    fn raise(&self) {
        self.flag.store(true, Ordering::Release);
        self.ctx.request_repaint();
    }
}

/// The window's drag source, if the desktop has one.
pub struct DragOut {
    missed: Missed,
    #[cfg(all(unix, not(target_os = "macos")))]
    wayland: Option<wayland::Dragger>,
    #[cfg(any(windows, target_os = "macos"))]
    native: Option<raw_window_handle::RawWindowHandle>,
    /// A drag is waiting for its file.
    waiting: bool,
    #[cfg(any(windows, target_os = "macos"))]
    wait_cursor: Option<egui::CustomCursorImage>,
}

impl DragOut {
    pub fn new(cc: &eframe::CreationContext<'_>) -> DragOut {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let window = cc.window_handle().map(|h| h.as_raw());
        let missed = Missed { flag: Arc::default(), ctx: cc.egui_ctx.clone() };
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            let display = cc.display_handle().map(|h| h.as_raw());
            let wayland = match (display, window) {
                (Ok(RawDisplayHandle::Wayland(d)), Ok(RawWindowHandle::Wayland(w))) => {
                    // SAFETY: winit's live wl_display and the window's wl_surface; both outlive
                    // the dragger, which `shutdown` ends from the app's `on_exit`.
                    match unsafe { wayland::Dragger::new(d.display.as_ptr(), w.surface.as_ptr(), missed.clone()) } {
                        Ok(d) => Some(d),
                        Err(e) => {
                            eprintln!("drag source: {e}");
                            None
                        }
                    }
                }
                _ => None,
            };
            DragOut { missed, wayland, waiting: false }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let native = match window {
                Ok(h @ (RawWindowHandle::Win32(_) | RawWindowHandle::AppKit(_))) => Some(h),
                _ => None,
            };
            // winit registers every window for file drops on macOS. kuulo takes none, and as a
            // drop target the window would accept its own drag and swallow the drop.
            #[cfg(target_os = "macos")]
            if let Some(RawWindowHandle::AppKit(h)) = native {
                // SAFETY: the window's live NSView, on the main thread that created it.
                let view = unsafe { &*(h.ns_view.as_ptr() as *const objc2_app_kit::NSView) };
                if let Some(window) = view.window() {
                    window.unregisterDraggedTypes();
                }
            }
            DragOut { missed, native, waiting: false, wait_cursor: None }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = window;
            DragOut { missed, waiting: false }
        }
    }

    /// The pointer while a drag waits for its file: `image`, or the busy cursor without one.
    /// Called every frame of the wait; `done_waiting` in the frame it ends.
    pub fn waiting(&mut self, ctx: &egui::Context, image: Option<&Image>) {
        self.waiting = true;
        ctx.set_cursor_icon(egui::CursorIcon::Progress);
        let Some(im) = image else { return };
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(d) = &self.wayland {
            match d.show_cursor(im) {
                Ok(true) => {}
                Ok(false) => ctx.request_repaint(),
                Err(e) => eprintln!("drag cursor: {e}"),
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let cursor = self.wait_cursor.get_or_insert_with(|| egui::CustomCursorImage { rgba: im.rgba.clone().into(), size: [im.width as u16, im.height as u16], hotspot: [0, 0] });
            ctx.set_cursor_image(Some(cursor.clone()));
        }
        #[cfg(not(any(unix, windows)))]
        let _ = im;
    }

    pub fn done_waiting(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.waiting) {
            return;
        }
        // egui-winit sets the window's cursor only when the icon changes, and the busy icon
        // can still stand from the wait.
        ctx.output_mut(|o| {
            if o.cursor_icon == egui::CursorIcon::Progress {
                o.cursor_icon = egui::CursorIcon::Default;
            }
        });
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(d) = &self.wayland {
            d.hide_cursor();
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.wait_cursor = None;
        }
    }

    /// Lets go of the Wayland connection while winit's display is still live; called once,
    /// as the app exits.
    pub fn shutdown(&mut self) {
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(d) = self.wayland.take() {
            d.shutdown();
        }
    }

    /// Whether a drag has ended without a drop since the last call. Windows reports only a
    /// drag called off with Escape.
    pub fn take_missed(&self) -> bool {
        self.missed.flag.swap(false, Ordering::AcqRel)
    }

    /// Whether the pointer's primary button is down, as the desktop has it now; `true` where
    /// it cannot say. egui can miss a release that comes after the pointer has left the window,
    /// and a Windows drag started once the button is up drops at once, wherever it is.
    pub fn button_held(&self) -> bool {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.wayland.as_ref().is_none_or(|d| d.button_held())
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
            use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
            // GetAsyncKeyState reads the physical buttons.
            let primary = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
            let state = unsafe { GetAsyncKeyState(primary.into()) };
            state < 0
        }
        #[cfg(target_os = "macos")]
        {
            objc2_app_kit::NSEvent::pressedMouseButtons() & 1 != 0
        }
        #[cfg(not(any(unix, windows)))]
        {
            true
        }
    }

    /// Starts dragging `paths`, written files, from the window, with `image` under the pointer.
    /// The pointer button must still be down.
    pub fn start(&self, paths: Vec<PathBuf>, image: Option<Image>) -> Result<(), String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            match &self.wayland {
                Some(d) => d.drag(paths, image),
                None => Err("dragging out of the window needs Wayland; on X11, use copy".into()),
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let Some(handle) = self.native else { return Err("no window handle for the drag".into()) };
            struct Native(raw_window_handle::RawWindowHandle);
            impl raw_window_handle::HasWindowHandle for Native {
                fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
                    // SAFETY: the handle is the live window's, held for the app's lifetime.
                    Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.0) })
                }
            }
            let missed = self.missed.clone();
            let ended = move |result, _| {
                if matches!(result, drag::DragResult::Cancel) {
                    missed.raise();
                }
            };
            drag::start_drag(&Native(handle), drag::DragItem::Files(paths), drag::Image::Raw(png(image)), ended, drag::Options::default()).map_err(|e| e.to_string())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (paths, image);
            Err("dragging out of the window is not available here; use copy".into())
        }
    }
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

#[cfg(all(unix, not(target_os = "macos")))]
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
