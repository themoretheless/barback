//! Enumerates menu bar items belonging to other processes via private CGS APIs.

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::geometry::CGRect;

use crate::cgs::{
    describe_windows, menu_bar_window_ids, on_screen_window_ids, window_frame, CGWindowID,
};

#[derive(Debug, Clone)]
pub struct MenuBarItem {
    pub window_id: CGWindowID,
    pub owner_pid: i32,
    pub owner_name: Option<String>,
    pub title: Option<String>,
    pub frame: Option<CGRect>,
    pub on_screen: bool,
}

impl MenuBarItem {
    pub fn display_name(&self) -> String {
        match (self.owner_name.as_deref(), self.title.as_deref()) {
            (Some(owner), Some(title)) if !title.is_empty() && owner != title => {
                format!("{owner} / {title}")
            }
            (Some(owner), _) => owner.to_string(),
            (None, Some(title)) if !title.is_empty() => title.to_string(),
            _ => format!("pid {}", self.owner_pid),
        }
    }
}

pub fn list() -> Vec<MenuBarItem> {
    let ids = menu_bar_window_ids();
    if ids.is_empty() {
        return Vec::new();
    }
    let on_screen: std::collections::HashSet<CGWindowID> =
        on_screen_window_ids().into_iter().collect();
    let Some(descriptions) = describe_windows(&ids) else {
        return Vec::new();
    };

    descriptions
        .iter()
        .filter_map(|dict| parse(&*dict, &on_screen))
        .collect()
}

fn parse(
    dict: &CFDictionary,
    on_screen: &std::collections::HashSet<CGWindowID>,
) -> Option<MenuBarItem> {
    let window_id = number(dict, "kCGWindowNumber")?.to_i64()? as CGWindowID;
    let owner_pid = number(dict, "kCGWindowOwnerPID")?.to_i32()?;
    let owner_name = string(dict, "kCGWindowOwnerName");
    let title = string(dict, "kCGWindowName");
    let frame = window_frame(window_id);

    Some(MenuBarItem {
        window_id,
        owner_pid,
        owner_name,
        title,
        frame,
        on_screen: on_screen.contains(&window_id),
    })
}

fn lookup(dict: &CFDictionary, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let ptr = dict.find(key.as_CFTypeRef() as *const _)?;
    unsafe { Some(CFType::wrap_under_get_rule(*ptr as _)) }
}

fn number(dict: &CFDictionary, key: &str) -> Option<CFNumber> {
    lookup(dict, key)?.downcast::<CFNumber>()
}

fn string(dict: &CFDictionary, key: &str) -> Option<String> {
    let value = lookup(dict, key)?.downcast::<CFString>()?;
    Some(value.to_string())
}
