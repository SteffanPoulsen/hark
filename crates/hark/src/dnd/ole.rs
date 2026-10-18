//! A drag of a file still being written, on Windows: OLE's drag loop with a data object that
//! names the file only when the drop target asks for it, once it is there. The `drag` crate's
//! file drag builds the shell's own data object from the path, which needs the file to exist.
//! Everything but the file list goes to an empty shell data object, which keeps what the drag
//! image helper stores and anything a target sets.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::core::{implement, Error, Result, HRESULT, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::CreateBitmap;
use windows::Win32::System::Com::*;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDragSourceHelper, SHCreateDataObject, SHCreateStdEnumFmtEtc, DROPFILES, SHDRAGIMAGE};

use super::{Image, Ready};

/// Runs the drag to its end, as OLE does, on the window's thread.
pub fn drag(paths: Vec<PathBuf>, image: Option<Image>, ready: Ready) -> std::result::Result<(), String> {
    unsafe {
        // The window registers no drop target, so winit leaves OLE to this; once initialised,
        // a repeat is a counted no-op.
        let _ = OleInitialize(None);
        let shell: IDataObject = SHCreateDataObject(None, None, None).map_err(|e| e.to_string())?;
        let data: IDataObject = Files { paths, ready, shell }.into();
        if let Some(picture) = image.as_ref().and_then(picture) {
            if let Ok(helper) = CoCreateInstance::<_, IDragSourceHelper>(&CLSID_DragDropHelper, None, CLSCTX_ALL) {
                let _ = helper.InitializeFromBitmap(&picture, &data);
            }
        }
        let source: IDropSource = Source.into();
        let mut effect = DROPEFFECT::default();
        let hr = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
        if hr.is_err() {
            return Err(Error::from(hr).to_string());
        }
        Ok(())
    }
}

/// The picture under the pointer, centred on it: straight-alpha BGRA, since the helper
/// premultiplies what it is given.
fn picture(im: &Image) -> Option<SHDRAGIMAGE> {
    let bgra: Vec<u8> = im.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    let bitmap = unsafe { CreateBitmap(im.width as i32, im.height as i32, 1, 32, Some(bgra.as_ptr() as *const c_void)) };
    if bitmap.is_invalid() {
        return None;
    }
    let (w, h) = (im.width as i32, im.height as i32);
    // No colour key: the card's dark pixels must not turn transparent.
    Some(SHDRAGIMAGE { sizeDragImage: SIZE { cx: w, cy: h }, ptOffset: POINT { x: w / 2, y: h / 2 }, hbmpDragImage: bitmap, crColorKey: COLORREF(0xFFFF_FFFF) })
}

fn file_list() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn is_file_list(format: *const FORMATETC) -> bool {
    unsafe { format.as_ref() }.is_some_and(|f| f.cfFormat == CF_HDROP.0 && f.dwAspect == DVASPECT_CONTENT.0 && f.tymed & TYMED_HGLOBAL.0 as u32 != 0)
}

#[implement(IDataObject)]
struct Files {
    paths: Vec<PathBuf>,
    ready: Ready,
    shell: IDataObject,
}

impl Files {
    /// `DROPFILES` followed by the wide paths, each ended by a nul and the list by another.
    fn drop_files(&self) -> Result<HGLOBAL> {
        let mut wide: Vec<u16> = Vec::new();
        for p in &self.paths {
            wide.extend(p.as_os_str().encode_wide());
            wide.push(0);
        }
        wide.push(0);
        let header = std::mem::size_of::<DROPFILES>();
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, header + wide.len() * 2)?;
            let at = GlobalLock(handle);
            if at.is_null() {
                return Err(Error::from_win32());
            }
            at.cast::<DROPFILES>().write(DROPFILES { pFiles: header as u32, pt: POINT::default(), fNC: FALSE, fWide: TRUE });
            std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), at.cast::<u8>().add(header), wide.len() * 2);
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for Files {
    fn GetData(&self, pformatetc: *const FORMATETC) -> Result<STGMEDIUM> {
        if !is_file_list(pformatetc) {
            return unsafe { self.shell.GetData(pformatetc) };
        }
        // The target is answered once the file is written; it waits, as a Wayland one does.
        if !self.ready.wait() {
            return Err(Error::new(E_FAIL, HSTRING::from("the file was not written")));
        }
        Ok(STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: self.drop_files()? }, pUnkForRelease: ManuallyDrop::new(None) })
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(Error::new(DV_E_FORMATETC, HSTRING::new()))
    }

    /// Answered at once, without waiting for the file: targets ask on every move over them.
    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if is_file_list(pformatetc) {
            S_OK
        } else {
            unsafe { self.shell.QueryGetData(pformatetc) }
        }
    }

    fn GetCanonicalFormatEtc(&self, _pformatectin: *const FORMATETC, pformatetcout: *mut FORMATETC) -> HRESULT {
        unsafe { (*pformatetcout).ptd = std::ptr::null_mut() };
        E_NOTIMPL
    }

    fn SetData(&self, pformatetc: *const FORMATETC, pmedium: *const STGMEDIUM, frelease: BOOL) -> Result<()> {
        unsafe { self.shell.SetData(pformatetc, pmedium, frelease) }
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(Error::new(E_NOTIMPL, HSTRING::new()));
        }
        unsafe { SHCreateStdEnumFmtEtc(&[file_list()]) }
    }

    fn DAdvise(&self, _pformatetc: *const FORMATETC, _advf: u32, _padvsink: Option<&IAdviseSink>) -> Result<u32> {
        Err(Error::new(OLE_E_ADVISENOTSUPPORTED, HSTRING::new()))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(Error::new(OLE_E_ADVISENOTSUPPORTED, HSTRING::new()))
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(Error::new(OLE_E_ADVISENOTSUPPORTED, HSTRING::new()))
    }
}

#[implement(IDropSource)]
struct Source;

#[allow(non_snake_case)]
impl IDropSource_Impl for Source {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if grfkeystate & MK_LBUTTON == MODIFIERKEYS_FLAGS(0) {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
