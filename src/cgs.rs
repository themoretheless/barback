//! Bindings to a handful of private CoreGraphics / SkyLight symbols.
//!
//! These are the same symbols Ice uses in `Ice/Bridging/Shims/Private.swift`.
//! They are undocumented and may change between macOS releases.

use std::ffi::c_int;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};

pub type CGSConnectionID = c_int;
pub type CGWindowID = u32;
pub type CGError = i32;

const KCG_ERROR_SUCCESS: CGError = 0;

unsafe extern "C" {
    fn CGSMainConnectionID() -> CGSConnectionID;

    fn CGSGetWindowCount(
        cid: CGSConnectionID,
        target_cid: CGSConnectionID,
        count: *mut c_int,
    ) -> CGError;

    fn CGSGetProcessMenuBarWindowList(
        cid: CGSConnectionID,
        target_cid: CGSConnectionID,
        count: c_int,
        list: *mut CGWindowID,
        out_count: *mut c_int,
    ) -> CGError;

    fn CGSGetOnScreenWindowList(
        cid: CGSConnectionID,
        target_cid: CGSConnectionID,
        count: c_int,
        list: *mut CGWindowID,
        out_count: *mut c_int,
    ) -> CGError;

    fn CGSGetScreenRectForWindow(
        cid: CGSConnectionID,
        wid: CGWindowID,
        out_rect: *mut CGRect,
    ) -> CGError;

    fn CGWindowListCreateDescriptionFromArray(window_array: CFArrayRef) -> CFArrayRef;
}

/// All menubar window ids across every running process.
pub fn menu_bar_window_ids() -> Vec<CGWindowID> {
    unsafe {
        let cid = CGSMainConnectionID();
        let mut total: c_int = 0;
        if CGSGetWindowCount(cid, 0, &mut total) != KCG_ERROR_SUCCESS || total <= 0 {
            return Vec::new();
        }
        let mut list = vec![0 as CGWindowID; total as usize];
        let mut real: c_int = 0;
        if CGSGetProcessMenuBarWindowList(cid, 0, total, list.as_mut_ptr(), &mut real)
            != KCG_ERROR_SUCCESS
        {
            return Vec::new();
        }
        list.truncate(real.max(0) as usize);
        list
    }
}

/// All window ids currently considered on-screen.
pub fn on_screen_window_ids() -> Vec<CGWindowID> {
    unsafe {
        let cid = CGSMainConnectionID();
        let mut total: c_int = 0;
        if CGSGetWindowCount(cid, 0, &mut total) != KCG_ERROR_SUCCESS || total <= 0 {
            return Vec::new();
        }
        let mut list = vec![0 as CGWindowID; total as usize];
        let mut real: c_int = 0;
        if CGSGetOnScreenWindowList(cid, 0, total, list.as_mut_ptr(), &mut real)
            != KCG_ERROR_SUCCESS
        {
            return Vec::new();
        }
        list.truncate(real.max(0) as usize);
        list
    }
}

pub fn window_frame(wid: CGWindowID) -> Option<CGRect> {
    let mut rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(0.0, 0.0));
    unsafe {
        let cid = CGSMainConnectionID();
        if CGSGetScreenRectForWindow(cid, wid, &mut rect) != KCG_ERROR_SUCCESS {
            return None;
        }
    }
    Some(rect)
}

/// Returns the `CGWindowListCopyWindowInfo`-shaped description for the given
/// window ids.
pub fn describe_windows(ids: &[CGWindowID]) -> Option<CFArray<CFDictionary>> {
    if ids.is_empty() {
        return None;
    }
    let cf_ids: CFArray<CGWindowID> = CFArray::from_copyable(ids);
    unsafe {
        let raw = CGWindowListCreateDescriptionFromArray(cf_ids.as_concrete_TypeRef());
        if raw.is_null() {
            return None;
        }
        Some(CFArray::<CFDictionary>::wrap_under_create_rule(raw))
    }
}
