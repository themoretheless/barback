#![cfg(target_os = "macos")]

mod cgs;
mod menu_bar_items;

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{declare_class, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSString};

struct ControllerIvars {
    status_item: Retained<NSStatusItem>,
    hidden: Cell<bool>,
}

declare_class!(
    struct Controller;

    unsafe impl ClassType for Controller {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "BarbackController";
    }

    impl DeclaredClass for Controller {
        type Ivars = ControllerIvars;
    }

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl Controller {
        #[method(toggleHidden:)]
        fn toggle_hidden(&self, _sender: Option<&AnyObject>) {
            let now_hidden = !self.ivars().hidden.get();
            self.ivars().hidden.set(now_hidden);
            self.refresh_title();
            self.dump_items();
            // TODO: synthesize ⌘-drag via CGEvent to actually move items.
        }

        #[method(dumpItems:)]
        fn dump_items_action(&self, _sender: Option<&AnyObject>) {
            self.dump_items();
        }

        #[method(quit:)]
        fn quit(&self, _sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::from(self);
            let app = NSApplication::sharedApplication(mtm);
            unsafe { app.terminate(None) };
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let status_bar = unsafe { NSStatusBar::systemStatusBar() };
        let status_item =
            unsafe { status_bar.statusItemWithLength(NSVariableStatusItemLength) };

        let this = mtm.alloc::<Self>().set_ivars(ControllerIvars {
            status_item,
            hidden: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send_id![super(this), init] };

        this.install_menu();
        this.refresh_title();
        this
    }

    fn install_menu(&self) {
        let mtm = MainThreadMarker::from(self);
        let menu = NSMenu::new(mtm);

        let toggle = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                ns_string!("Toggle Hidden Items"),
                Some(sel!(toggleHidden:)),
                ns_string!("h"),
            )
        };
        unsafe { toggle.setTarget(Some(self)) };
        menu.addItem(&toggle);

        let dump = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                ns_string!("Log Menubar Items"),
                Some(sel!(dumpItems:)),
                ns_string!("l"),
            )
        };
        unsafe { dump.setTarget(Some(self)) };
        menu.addItem(&dump);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                ns_string!("Quit Barback"),
                Some(sel!(quit:)),
                ns_string!("q"),
            )
        };
        unsafe { quit.setTarget(Some(self)) };
        menu.addItem(&quit);

        unsafe { self.ivars().status_item.setMenu(Some(&menu)) };
    }

    fn dump_items(&self) {
        let items = menu_bar_items::list();
        println!("--- {} menubar items ---", items.len());
        for item in &items {
            let (x, w) = match item.frame {
                Some(f) => (f.origin.x, f.size.width),
                None => (f64::NAN, f64::NAN),
            };
            println!(
                "  [{:>9}] pid={:>6} on={:5} x={:>7.1} w={:>6.1} {}",
                item.window_id,
                item.owner_pid,
                item.on_screen,
                x,
                w,
                item.display_name(),
            );
        }
    }

    fn refresh_title(&self) {
        let title: &NSString = if self.ivars().hidden.get() {
            ns_string!("▶")
        } else {
            ns_string!("◀")
        };
        if let Some(button) = unsafe { self.ivars().status_item.button(MainThreadMarker::from(self)) } {
            unsafe { button.setTitle(title) };
        }
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Keep the controller alive for the lifetime of the app.
    let _controller = Controller::new(mtm);
    std::mem::forget(_controller);

    unsafe { app.run() };
}
