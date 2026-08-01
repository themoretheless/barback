//! The AppKit shell: one status item for the hide/show arrow, an optional second
//! one for the next meeting, and the dropdown that shows both.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{declare_class, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_event_kit::{EKEventStore, EKEventStoreChangedNotification};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSCalendarDayChangedNotification, NSNotificationCenter,
    NSObjectNSThreadPerformAdditions, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes, NSString,
    NSSystemTimeZoneDidChangeNotification, NSTimer, NSURL,
};

use crate::calendar;
use crate::meeting::{self, Access, Config, HeaderAction, MenuModel, RawEvent};
use crate::menu_bar_items;

/// Refetch at most this often when the menu is opened repeatedly.
const FETCH_TTL: i64 = 30;

const PRIVACY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars";

struct ControllerIvars {
    status_item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    hidden: Cell<bool>,

    store: Retained<EKEventStore>,
    config: Config,
    access: Cell<Access>,
    loading: Cell<bool>,
    events: RefCell<Vec<RawEvent>>,
    fetched_at: Cell<i64>,

    /// The optional second status item that shows the meeting in the menu bar.
    label_item: RefCell<Option<Retained<NSStatusItem>>>,
    label_menu: Retained<NSMenu>,
    show_label: Cell<bool>,
    tick: RefCell<Option<Retained<NSTimer>>>,
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

    unsafe impl NSMenuDelegate for Controller {
        // AppKit's "populate yourself now" hook. It runs before the menu is laid
        // out, so titles set here are measured correctly.
        #[method(menuNeedsUpdate:)]
        fn menu_needs_update(&self, _menu: &NSMenu) {
            self.refresh(false);
        }
    }

    unsafe impl Controller {
        #[method(toggleHidden:)]
        fn toggle_hidden(&self, _sender: Option<&AnyObject>) {
            let now_hidden = !self.ivars().hidden.get();
            self.ivars().hidden.set(now_hidden);
            self.refresh_arrow();
            self.dump_items();
            // TODO: synthesize the command-drag via CGEvent to actually move items.
        }

        #[method(dumpItems:)]
        fn dump_items_action(&self, _sender: Option<&AnyObject>) {
            self.dump_items();
        }

        #[method(toggleMenuBarLabel:)]
        fn toggle_menu_bar_label(&self, _sender: Option<&AnyObject>) {
            let show = !self.ivars().show_label.get();
            self.ivars().show_label.set(show);
            self.apply_label_visibility();
            self.refresh(false);
        }

        #[method(requestCalendarAccess:)]
        fn request_calendar_access(&self, _sender: Option<&AnyObject>) {
            self.ivars().loading.set(true);
            let target: *const NSObject = &**self;
            calendar::request_access(
                &self.ivars().store,
                target,
                sel!(calendarAccessGranted),
                sel!(calendarAccessDenied),
            );
        }

        #[method(calendarAccessGranted)]
        fn calendar_access_granted(&self) {
            self.ivars().loading.set(false);
            self.ivars().access.set(calendar::access());
            self.refresh(true);
        }

        #[method(calendarAccessDenied)]
        fn calendar_access_denied(&self) {
            self.ivars().loading.set(false);
            self.ivars().access.set(calendar::access());
            self.refresh(false);
        }

        #[method(openPrivacySettings:)]
        fn open_privacy_settings(&self, _sender: Option<&AnyObject>) {
            let text = NSString::from_str(PRIVACY_SETTINGS_URL);
            if let Some(url) = unsafe { NSURL::URLWithString(&text) } {
                unsafe { NSWorkspace::sharedWorkspace().openURL(&url) };
            }
        }

        // NSNotificationCenter delivers synchronously on whichever thread posted
        // the notification, and EventKit posts from its own worker. These two
        // selectors must therefore touch no ivar and no AppKit: all they may do
        // is hop to the main thread, where the ivars are not shared and the
        // MainThreadMarker is honest.
        #[method(calendarsChanged:)]
        fn calendars_changed(&self, _note: Option<&AnyObject>) {
            self.hop_to_main(sel!(calendarsChangedOnMain));
        }

        #[method(environmentChanged:)]
        fn environment_changed(&self, _note: Option<&AnyObject>) {
            self.hop_to_main(sel!(environmentChangedOnMain));
        }

        // EventKit hands out snapshots, so a change means refetch, not reread.
        #[method(calendarsChangedOnMain)]
        fn calendars_changed_on_main(&self) {
            calendar::reset(&self.ivars().store);
            self.refresh(true);
        }

        // The machine may have been asleep for days, and a timezone or day change
        // moves the boundaries every label is measured against.
        #[method(environmentChangedOnMain)]
        fn environment_changed_on_main(&self) {
            self.refresh(true);
        }

        #[method(tick:)]
        fn tick(&self, _timer: Option<&AnyObject>) {
            self.refresh(false);
        }

        #[method(quit:)]
        fn quit(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let app = NSApplication::sharedApplication(mtm);
            unsafe { app.terminate(None) };
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let status_bar = unsafe { NSStatusBar::systemStatusBar() };
        let status_item = unsafe { status_bar.statusItemWithLength(NSVariableStatusItemLength) };

        let this = mtm.alloc::<Self>().set_ivars(ControllerIvars {
            status_item,
            menu: NSMenu::new(mtm),
            hidden: Cell::new(false),

            store: calendar::new_store(),
            config: Config::default(),
            access: Cell::new(calendar::access()),
            loading: Cell::new(false),
            events: RefCell::new(Vec::new()),
            fetched_at: Cell::new(i64::MIN),

            label_item: RefCell::new(None),
            label_menu: NSMenu::new(mtm),
            show_label: Cell::new(false),
            tick: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send_id![super(this), init] };

        this.install_menus();
        this.refresh_arrow();
        this.observe_notifications();
        this.refresh(true);
        this
    }

    /// `MainThreadMarker::from(self)` is a type-system-only construct: it calls
    /// `new_unchecked` and inserts no runtime check, so getting it wrong is
    /// silent undefined behaviour rather than a crash. Assert instead.
    fn mtm(&self) -> MainThreadMarker {
        MainThreadMarker::new().expect("AppKit touched off the main thread")
    }

    /// Enqueues `selector` on the main run loop and returns immediately.
    ///
    /// Called from notification selectors that may arrive on a background
    /// thread. It is sound there only because it reads no ivar and calls no
    /// AppKit: everything else on `Controller` is main thread only.
    fn hop_to_main(&self, selector: objc2::runtime::Sel) {
        unsafe {
            self.performSelectorOnMainThread_withObject_waitUntilDone(selector, None, false);
        }
    }

    fn install_menus(&self) {
        let ivars = self.ivars();
        let delegate = ProtocolObject::from_ref(self);
        for menu in [&ivars.menu, &ivars.label_menu] {
            // Without this AppKit recomputes every item's enabled state from its
            // target and action each time the menu is shown, which would undo
            // setEnabled(false) on the informational rows.
            unsafe { menu.setAutoenablesItems(false) };
            unsafe { menu.setDelegate(Some(delegate)) };
        }
        unsafe { ivars.status_item.setMenu(Some(&ivars.menu)) };
    }

    fn observe_notifications(&self) {
        let observer: &AnyObject = self.as_ref();

        let center = unsafe { NSNotificationCenter::defaultCenter() };
        unsafe {
            center.addObserver_selector_name_object(
                observer,
                sel!(calendarsChanged:),
                Some(EKEventStoreChangedNotification),
                None,
            );
            center.addObserver_selector_name_object(
                observer,
                sel!(environmentChanged:),
                Some(NSCalendarDayChangedNotification),
                None,
            );
            center.addObserver_selector_name_object(
                observer,
                sel!(environmentChanged:),
                Some(NSSystemTimeZoneDidChangeNotification),
                None,
            );
        }

        // Sleep and wake are posted on the workspace's own center, not the
        // default one.
        let workspace_center = unsafe { NSWorkspace::sharedWorkspace().notificationCenter() };
        unsafe {
            workspace_center.addObserver_selector_name_object(
                observer,
                sel!(environmentChanged:),
                Some(objc2_app_kit::NSWorkspaceDidWakeNotification),
                None,
            );
        }
    }

    // -- data -------------------------------------------------------------

    fn refresh(&self, force_fetch: bool) {
        let ivars = self.ivars();
        let now = calendar::now_snapshot();

        ivars.access.set(calendar::access());
        if ivars.access.get() == Access::FullAccess {
            let stale = now.instant.saturating_sub(ivars.fetched_at.get()) >= FETCH_TTL;
            if force_fetch || stale {
                let fetched = calendar::fetch(&ivars.store, now);
                *ivars.events.borrow_mut() = fetched;
                ivars.fetched_at.set(now.instant);
                ivars.loading.set(false);
            }
        } else {
            ivars.events.borrow_mut().clear();
            ivars.fetched_at.set(i64::MIN);
        }

        // Keep the borrow short: rebuilding the menu re-enters Objective-C.
        let model = {
            let events = ivars.events.borrow();
            meeting::build_menu(
                &events,
                now,
                ivars.access.get(),
                ivars.loading.get(),
                &ivars.config,
            )
        };
        let next_change = {
            let events = ivars.events.borrow();
            meeting::next_change_at(&events, now, &ivars.config)
        };


        let mtm = self.mtm();
        self.populate(&ivars.menu, &model, mtm);
        self.populate(&ivars.label_menu, &model, mtm);

        if ivars.show_label.get() {
            let title = meeting::status_title(&model.state, now, &ivars.config);
            self.set_label_title(&title);
        }

        self.reschedule_tick(next_change.map(|t| (t - now.instant).max(1)));
    }

    // -- menu -------------------------------------------------------------

    fn info_item(&self, mtm: MainThreadMarker, text: &str, indent: isize) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                &NSString::from_str(text),
                None,
                ns_string!(""),
            )
        };
        unsafe { item.setEnabled(false) };
        if indent > 0 {
            unsafe { item.setIndentationLevel(indent) };
        }
        item
    }

    fn action_item(
        &self,
        mtm: MainThreadMarker,
        text: &str,
        selector: objc2::runtime::Sel,
        key: &NSString,
        indent: isize,
    ) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                &NSString::from_str(text),
                Some(selector),
                key,
            )
        };
        unsafe { item.setTarget(Some(self)) };
        if indent > 0 {
            unsafe { item.setIndentationLevel(indent) };
        }
        item
    }

    fn populate(&self, menu: &NSMenu, model: &MenuModel, mtm: MainThreadMarker) {
        unsafe { menu.removeAllItems() };

        let header = &model.header;
        let title_item = self.info_item(mtm, &header.primary, 0);
        if let Some(tip) = &header.tooltip {
            unsafe { title_item.setToolTip(Some(&NSString::from_str(tip))) };
        }
        menu.addItem(&title_item);

        match header.action {
            HeaderAction::None => {
                if let Some(secondary) = &header.secondary {
                    menu.addItem(&self.info_item(mtm, secondary, 1));
                }
            }
            HeaderAction::RequestAccess => {
                let text = header.secondary.as_deref().unwrap_or("Grant Calendar Access");
                menu.addItem(&self.action_item(
                    mtm,
                    text,
                    sel!(requestCalendarAccess:),
                    ns_string!(""),
                    1,
                ));
            }
            HeaderAction::OpenSettings => {
                let text = header.secondary.as_deref().unwrap_or("Open Privacy Settings");
                menu.addItem(&self.action_item(
                    mtm,
                    text,
                    sel!(openPrivacySettings:),
                    ns_string!(""),
                    1,
                ));
            }
        }
        if let Some(tertiary) = &header.tertiary {
            menu.addItem(&self.info_item(mtm, tertiary, 1));
        }

        let sections: [(&str, &Vec<meeting::MenuRow>, usize); 3] = [
            ("All day", &model.all_day, 0),
            ("Today", &model.today, model.today_truncated),
            ("Tomorrow", &model.tomorrow, model.tomorrow_truncated),
        ];
        for (name, rows, dropped) in sections {
            if rows.is_empty() {
                continue;
            }
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            menu.addItem(&self.info_item(mtm, name, 0));
            for row in rows {
                // The selected meeting is the one the header already leads with;
                // marking it keeps the overlap resolution legible.
                let marker = if row.is_selected { "\u{25b8} " } else { "" };
                let dimmed = if row.dimmed { " (not accepted)" } else { "" };
                let item = self.info_item(
                    mtm,
                    &format!("{marker}{}{dimmed}", row.title),
                    1,
                );
                unsafe { item.setToolTip(Some(&NSString::from_str(&row.tooltip))) };
                menu.addItem(&item);
                menu.addItem(&self.info_item(mtm, &row.detail, 2));
            }
            if dropped > 0 {
                menu.addItem(&self.info_item(mtm, &format!("+{dropped} more"), 1));
            }
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let label_title = if self.ivars().show_label.get() {
            "Hide Meeting in Menu Bar"
        } else {
            "Show Meeting in Menu Bar"
        };
        menu.addItem(&self.action_item(
            mtm,
            label_title,
            sel!(toggleMenuBarLabel:),
            ns_string!(""),
            0,
        ));
        menu.addItem(&self.action_item(
            mtm,
            "Toggle Hidden Items",
            sel!(toggleHidden:),
            ns_string!("h"),
            0,
        ));
        menu.addItem(&self.action_item(
            mtm,
            "Log Menubar Items",
            sel!(dumpItems:),
            ns_string!("l"),
            0,
        ));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.action_item(mtm, "Quit Barback", sel!(quit:), ns_string!("q"), 0));
    }

    // -- status items -----------------------------------------------------

    fn refresh_arrow(&self) {
        let title: &NSString = if self.ivars().hidden.get() {
            ns_string!("\u{25b6}")
        } else {
            ns_string!("\u{25c0}")
        };
        let mtm = self.mtm();
        if let Some(button) = unsafe { self.ivars().status_item.button(mtm) } {
            unsafe { button.setTitle(title) };
        }
    }

    fn apply_label_visibility(&self) {
        let ivars = self.ivars();
        if ivars.show_label.get() {
            let mut slot = ivars.label_item.borrow_mut();
            if slot.is_none() {
                let bar = unsafe { NSStatusBar::systemStatusBar() };
                let item = unsafe { bar.statusItemWithLength(NSVariableStatusItemLength) };
                unsafe { item.setMenu(Some(&ivars.label_menu)) };
                *slot = Some(item);
            }
        } else {
            // Drop the item so the slot is returned to the menu bar. This app
            // exists to reclaim menu bar space, so it must not squat in it.
            if let Some(item) = ivars.label_item.borrow_mut().take() {
                unsafe { item.setVisible(false) };
                unsafe { NSStatusBar::systemStatusBar().removeStatusItem(&item) };
            }
        }
    }

    fn set_label_title(&self, text: &str) {
        let mtm = self.mtm();
        let slot = self.ivars().label_item.borrow();
        let Some(item) = slot.as_ref() else {
            return;
        };
        if let Some(button) = unsafe { item.button(mtm) } {
            unsafe { button.setTitle(&NSString::from_str(text)) };
        }
    }

    /// One wakeup per visible change instead of one per minute. The timer is
    /// registered in the common run loop modes so the countdown keeps moving
    /// while the menu is open.
    fn reschedule_tick(&self, seconds: Option<i64>) {
        let ivars = self.ivars();
        if let Some(old) = ivars.tick.borrow_mut().take() {
            unsafe { old.invalidate() };
        }
        // Nothing to tick unless the label is on screen.
        if !ivars.show_label.get() {
            return;
        }
        let Some(seconds) = seconds else {
            return;
        };

        let interval = seconds.clamp(1, 300) as f64;
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                interval,
                self,
                sel!(tick:),
                None,
                false,
            )
        };
        // Let the kernel coalesce this with other wakeups.
        unsafe { timer.setTolerance((interval * 0.1).min(15.0)) };
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        *ivars.tick.borrow_mut() = Some(timer);
    }

    // -- existing behaviour -----------------------------------------------

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
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Keep the controller alive for the lifetime of the app. Several places rely
    // on this: the notification observers are never removed, and the EventKit
    // completion block holds a bare pointer back to it.
    let controller = Controller::new(mtm);
    std::mem::forget(controller);

    unsafe { app.run() };
}
