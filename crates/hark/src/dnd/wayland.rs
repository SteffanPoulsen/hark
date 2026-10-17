//! A drag source on winit's own Wayland connection. winit has none, so this binds its own
//! `wl_seat`, `wl_pointer` and `wl_data_device` on the same `wl_display`, with an event queue
//! of its own on its own thread. The pointer sees the same button presses as winit's, which
//! gives the serial the compositor wants for a drag; the drag itself starts from the window
//! thread, and the source's events (what to send, when it is over) come back on the queue
//! thread. The picture under the pointer is a surface of its own with a shared-memory buffer.
//! Copy only: the file stays where it is.

use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};

use super::Image;

const BTN_LEFT: u32 = 0x110;
const NO_PRESS: u64 = u64::MAX;

pub struct Dragger {
    conn: Connection,
    qh: QueueHandle<State>,
    manager: wl_data_device_manager::WlDataDeviceManager,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    device: wl_data_device::WlDataDevice,
    surface: wl_surface::WlSurface,
    /// Serial of the left press being held, or `NO_PRESS`.
    press: Arc<AtomicU64>,
    dragging: Arc<AtomicBool>,
}

struct State {
    pointer: Option<wl_pointer::WlPointer>,
    press: Arc<AtomicU64>,
    dragging: Arc<AtomicBool>,
}

/// What a source in flight sends, the icon it carries, and whether the file is written yet.
struct Payload {
    paths: Vec<PathBuf>,
    icon: Option<(wl_surface::WlSurface, wl_buffer::WlBuffer)>,
    ready: Option<super::Ready>,
}

impl Dragger {
    /// # Safety
    /// `display` and `surface` are winit's live `wl_display` and the window's `wl_surface`,
    /// and outlive the dragger.
    pub unsafe fn new(display: *mut std::ffi::c_void, surface: *mut std::ffi::c_void) -> Result<Dragger, String> {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let manager = globals.bind::<wl_data_device_manager::WlDataDeviceManager, _, _>(&qh, 1..=3, ()).map_err(|e| format!("wl_data_device_manager: {e}"))?;
        let compositor = globals.bind::<wl_compositor::WlCompositor, _, _>(&qh, 1..=4, ()).map_err(|e| format!("wl_compositor: {e}"))?;
        let shm = globals.bind::<wl_shm::WlShm, _, _>(&qh, 1..=1, ()).map_err(|e| format!("wl_shm: {e}"))?;
        let seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=7, ()).map_err(|e| format!("wl_seat: {e}"))?;
        let device = manager.get_data_device(&seat, &qh, ());
        let press = Arc::new(AtomicU64::new(NO_PRESS));
        let dragging = Arc::new(AtomicBool::new(false));
        let mut state = State { pointer: None, press: press.clone(), dragging: dragging.clone() };
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        if state.pointer.is_none() {
            return Err("the seat has no pointer".into());
        }
        let id = unsafe { ObjectId::from_ptr(wl_surface::WlSurface::interface(), surface.cast()) }.map_err(|e| e.to_string())?;
        let surface = wl_surface::WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("wl-drag".into())
            .spawn(move || while queue.blocking_dispatch(&mut state).is_ok() {})
            .map_err(|e| e.to_string())?;
        Ok(Dragger { conn, qh, manager, compositor, shm, device, surface, press, dragging })
    }

    /// Drags `paths` from the window. The compositor only starts a drag for the press it is
    /// grabbing, so the button must still be down.
    pub fn drag(&self, paths: Vec<PathBuf>, image: Option<Image>, ready: Option<super::Ready>) -> Result<(), String> {
        let serial = match self.press.load(Ordering::Acquire) {
            NO_PRESS => return Err("no button held; the drag did not start".into()),
            s => s as u32,
        };
        let icon = image.and_then(|im| self.icon(&im).map_err(|e| eprintln!("drag icon: {e}")).ok());
        let source = self.manager.create_data_source(&self.qh, Payload { paths, icon: icon.clone(), ready });
        source.offer("text/uri-list".into());
        if source.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        self.dragging.store(true, Ordering::Release);
        self.device.start_drag(Some(&source), &self.surface, icon.as_ref().map(|(s, _)| s), serial);
        if let Some((surface, buffer)) = &icon {
            surface.attach(Some(buffer), 0, 0);
            surface.commit();
        }
        self.conn.flush().map_err(|e| e.to_string())
    }

    /// A surface showing `im`, from a buffer in a file the compositor maps.
    fn icon(&self, im: &Image) -> Result<(wl_surface::WlSurface, wl_buffer::WlBuffer), String> {
        let (w, h) = (im.width as i32, im.height as i32);
        let stride = w * 4;
        let size = stride * h;
        let mut argb = Vec::with_capacity(size as usize);
        for px in im.rgba.as_chunks::<4>().0 {
            let a = px[3] as u32;
            let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            argb.extend_from_slice(&[pre(px[2]), pre(px[1]), pre(px[0]), px[3]]);
        }
        let name = format!("/dev/shm/hark-drag-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let mut file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&name).map_err(|e| format!("{name}: {e}"))?;
        let _ = std::fs::remove_file(&name);
        file.write_all(&argb).map_err(|e| e.to_string())?;
        let pool = self.shm.create_pool(file.as_fd(), size, &self.qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, &self.qh, ());
        pool.destroy();
        let surface = self.compositor.create_surface(&self.qh, ());
        if surface.version() >= 3 {
            surface.set_buffer_scale(im.scale.max(1));
        }
        if surface.version() >= 4 {
            surface.damage_buffer(0, 0, w, h);
        } else {
            surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        Ok((surface, buffer))
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &wl_data_device_manager::WlDataDeviceManager, _: wl_data_device_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: wl_compositor::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(st: &mut Self, seat: &wl_seat::WlSeat, ev: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = ev else { return };
        let has_pointer = caps.contains(wl_seat::Capability::Pointer);
        if has_pointer && st.pointer.is_none() {
            st.pointer = Some(seat.get_pointer(qh, ()));
        } else if !has_pointer {
            if let Some(p) = st.pointer.take() {
                if p.version() >= 3 {
                    p.release();
                }
            }
            st.press.store(NO_PRESS, Ordering::Release);
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Button { serial, button: BTN_LEFT, state, .. } => {
                let pressed = state == WEnum::Value(wl_pointer::ButtonState::Pressed);
                st.press.store(if pressed { serial as u64 } else { NO_PRESS }, Ordering::Release);
            }
            wl_pointer::Event::Leave { .. } if !st.dragging.load(Ordering::Acquire) => st.press.store(NO_PRESS, Ordering::Release),
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, Payload> for State {
    fn event(st: &mut Self, source: &wl_data_source::WlDataSource, ev: wl_data_source::Event, payload: &Payload, _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_data_source::Event::Send { mime_type, fd } if mime_type == "text/uri-list" => {
                // The receiver waits on the pipe, so the file is complete before it is named.
                if payload.ready.as_ref().is_some_and(|r| !r.wait()) {
                    return;
                }
                let mut file = std::fs::File::from(fd);
                let _ = file.write_all(super::uri_list(&payload.paths).as_bytes());
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                st.dragging.store(false, Ordering::Release);
                st.press.store(NO_PRESS, Ordering::Release);
                if let Some((surface, buffer)) = &payload.icon {
                    surface.destroy();
                    buffer.destroy();
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

// The device is ours only for starting drags; offers made to the window are winit's business,
// and the ones mirrored here are let go as they arrive.
impl Dispatch<wl_data_device::WlDataDevice, ()> for State {
    fn event(_: &mut Self, _: &wl_data_device::WlDataDevice, ev: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_data_device::Event::Enter { id: Some(offer), .. } | wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
    ]);
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for State {
    fn event(_: &mut Self, _: &wl_data_offer::WlDataOffer, _: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
