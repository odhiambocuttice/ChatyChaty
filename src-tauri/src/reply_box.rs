//! Finds the chat's typing box through the accessibility tree, so the draft is
//! typed into the right field without asking the model where the box is.
//!
//! On Windows this is UI Automation (the same tree `reply` reads the chat's
//! text from); on macOS it is the AX API that the accessibility permission
//! unlocks (the same one typing needs). Browsers expose a page's text areas
//! through both. A remote-desktop window exposes nothing, and then the caller
//! falls back to the position the draft came with.

/// A text field on screen, in the units the mouse is moved in: logical points
/// on macOS, physical pixels on Windows, with the origin at the top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplyBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ReplyBox {
    pub fn center(&self) -> (i32, i32) {
        (
            (self.x + self.width / 2.0).round() as i32,
            (self.y + self.height / 2.0).round() as i32,
        )
    }

    fn bottom(&self) -> f64 {
        self.y + self.height
    }
}

/// A window's frame, in the same units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Narrower fields are search boxes and the like, not a message box.
const MIN_WIDTH: f64 = 120.0;
const MIN_HEIGHT: f64 = 18.0;
/// How many accessibility nodes to look at before giving up: a long chat page
/// has thousands, and the box is always reached well within this.
#[allow(dead_code)]
const MAX_NODES: usize = 6000;

/// The chat's typing box among a window's editable fields: the lowest one that
/// is inside the window, wide enough to be a message box, and not so tall that
/// it is the page itself (a document view). None when there is none.
pub fn pick(fields: &[ReplyBox], window: Frame) -> Option<ReplyBox> {
    fields
        .iter()
        .filter(|f| f.width >= MIN_WIDTH && f.height >= MIN_HEIGHT)
        .filter(|f| f.height <= window.height / 2.0)
        .filter(|f| {
            let (cx, cy) = (f.x + f.width / 2.0, f.y + f.height / 2.0);
            cx >= window.x
                && cx <= window.x + window.width
                && cy >= window.y
                && cy <= window.y + window.height
        })
        .max_by(|a, b| a.bottom().total_cmp(&b.bottom()))
        .copied()
}

/// Whether the element with keyboard focus is an editable text field. None
/// when the accessibility tree cannot say (no permission, no tree). On macOS
/// the front app (`front_app_pid`) is asked first, then the system.
pub fn focus_is_editable(front_app_pid: Option<i32>) -> Option<bool> {
    platform::focus_is_editable(front_app_pid)
}

/// The front window's typing box. `front_app_pid` is the app in front on
/// macOS (read on the main thread); Windows reads the front window itself.
pub fn find_reply_box(front_app_pid: Option<i32>) -> Option<ReplyBox> {
    platform::find_reply_box(front_app_pid)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{pick, Frame, ReplyBox, MAX_NODES};
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFRelease, CFRetain, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::geometry::{CGPoint, CGSize};
    use std::ffi::c_void;

    type AXUIElementRef = CFTypeRef;
    const K_AX_VALUE_CG_POINT_TYPE: u32 = 1;
    const K_AX_VALUE_CG_SIZE_TYPE: u32 = 2;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> i32;
    }

    /// Chromium browsers (Chrome, Brave, Edge) build a page's accessibility
    /// tree only once a client asks for it through this attribute; without it
    /// the window shows no text fields at all. Other apps ignore it.
    fn enable_browser_accessibility(app: CFTypeRef) {
        use core_foundation::boolean::CFBoolean;
        let yes = CFBoolean::true_value();
        for attribute in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
            let name = CFString::new(attribute);
            let status = unsafe {
                AXUIElementSetAttributeValue(app, name.as_concrete_TypeRef(), yes.as_CFTypeRef())
            };
            log::debug!("Setting {attribute} on the front app: status {status}");
        }
    }

    /// Whether this process may read other apps' accessibility trees (the
    /// permission Chatelle asks for at first start). Logged when the box is
    /// not found, so a missing permission is not mistaken for a missing box.
    pub fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// A Core Foundation object this module owns and releases.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn attribute(element: CFTypeRef, name: &str) -> Option<Owned> {
        let name = CFString::new(name);
        let mut value: CFTypeRef = std::ptr::null();
        let status = unsafe {
            AXUIElementCopyAttributeValue(element, name.as_concrete_TypeRef(), &mut value)
        };
        (status == 0 && !value.is_null()).then_some(Owned(value))
    }

    fn string_attribute(element: CFTypeRef, name: &str) -> Option<String> {
        let value = attribute(element, name)?;
        // Borrowed for the read; `value` releases it.
        let string = unsafe { CFString::wrap_under_get_rule(value.0 as CFStringRef) };
        Some(string.to_string())
    }

    fn children(element: CFTypeRef) -> Vec<Owned> {
        let Some(value) = attribute(element, "AXChildren") else {
            return Vec::new();
        };
        let array: CFArray<CFTypeRef> =
            unsafe { CFArray::wrap_under_get_rule(value.0 as core_foundation::array::CFArrayRef) };
        array
            .get_all_values()
            .into_iter()
            .map(|child| {
                // The array owns its members; keep each one alive on its own.
                unsafe { CFRetain(child) };
                Owned(child)
            })
            .collect()
    }

    fn frame(element: CFTypeRef) -> Option<Frame> {
        let position = attribute(element, "AXPosition")?;
        let size = attribute(element, "AXSize")?;
        let mut point = CGPoint::new(0.0, 0.0);
        let mut extent = CGSize::new(0.0, 0.0);
        let ok = unsafe {
            AXValueGetValue(
                position.0,
                K_AX_VALUE_CG_POINT_TYPE,
                &mut point as *mut CGPoint as *mut c_void,
            ) && AXValueGetValue(
                size.0,
                K_AX_VALUE_CG_SIZE_TYPE,
                &mut extent as *mut CGSize as *mut c_void,
            )
        };
        ok.then_some(Frame {
            x: point.x,
            y: point.y,
            width: extent.width,
            height: extent.height,
        })
    }

    fn is_text_field(role: &str) -> bool {
        matches!(
            role,
            "AXTextArea" | "AXTextField" | "AXSearchField" | "AXComboBox"
        )
    }

    pub fn focus_is_editable(front_app_pid: Option<i32>) -> Option<bool> {
        if !trusted() {
            return None;
        }
        let focused = front_app_pid
            .and_then(|pid| {
                let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
                attribute(app.0, "AXFocusedUIElement")
            })
            .or_else(|| {
                let system = Owned(unsafe { AXUIElementCreateSystemWide() });
                attribute(system.0, "AXFocusedUIElement")
            })?;
        let role = string_attribute(focused.0, "AXRole")?;
        Some(is_text_field(&role))
    }

    /// What the live test sees: the focused window's frame, how many nodes
    /// were visited, and the roles met, most common first.
    #[cfg(test)]
    #[derive(Debug)]
    pub struct Survey {
        pub window: Option<Frame>,
        pub nodes: usize,
        pub roles: Vec<(String, usize)>,
        pub fields: Vec<ReplyBox>,
    }

    #[cfg(test)]
    pub fn survey(pid: i32) -> Survey {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        enable_browser_accessibility(app.0);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        println!("  app title: {:?}", string_attribute(app.0, "AXTitle"));
        for name in [
            "AXFocusedWindow",
            "AXMainWindow",
            "AXWindows",
            "AXFocusedUIElement",
        ] {
            match attribute(app.0, name) {
                Some(v) => println!(
                    "  {name}: role={:?} frame={:?} children={}",
                    string_attribute(v.0, "AXRole"),
                    frame(v.0),
                    children(v.0).len()
                ),
                None => println!("  {name}: (none)"),
            }
        }
        let mut out = Survey {
            window: None,
            nodes: 0,
            roles: Vec::new(),
            fields: Vec::new(),
        };
        let window = attribute(app.0, "AXFocusedWindow")
            .or_else(|| attribute(app.0, "AXMainWindow"))
            .or_else(|| children(app.0).into_iter().next());
        let Some(window) = window else {
            return out;
        };
        out.window = frame(window.0);
        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        let mut pending = vec![window];
        while let Some(element) = pending.pop() {
            out.nodes += 1;
            if out.nodes > MAX_NODES {
                break;
            }
            let role = string_attribute(element.0, "AXRole").unwrap_or_default();
            *counts.entry(role.clone()).or_default() += 1;
            if is_text_field(&role) {
                if let Some(f) = frame(element.0) {
                    out.fields.push(ReplyBox {
                        x: f.x,
                        y: f.y,
                        width: f.width,
                        height: f.height,
                    });
                }
                continue;
            }
            pending.extend(children(element.0));
        }
        out.roles = counts.into_iter().collect();
        out.roles.sort_by_key(|a| std::cmp::Reverse(a.1));
        out.roles.truncate(12);
        out
    }

    pub fn find_reply_box(front_app_pid: Option<i32>) -> Option<ReplyBox> {
        if !trusted() {
            log::warn!("No accessibility permission, so the typing box cannot be looked up");
            return None;
        }
        let app = Owned(unsafe { AXUIElementCreateApplication(front_app_pid?) });
        if app.0.is_null() {
            return None;
        }
        enable_browser_accessibility(app.0);
        let window = attribute(app.0, "AXFocusedWindow")?;
        let window_frame = frame(window.0)?;
        let mut fields = Vec::new();
        let mut pending = vec![window];
        let mut seen = 0;
        while let Some(element) = pending.pop() {
            seen += 1;
            if seen > MAX_NODES {
                break;
            }
            let role = string_attribute(element.0, "AXRole").unwrap_or_default();
            if is_text_field(&role) {
                if let Some(f) = frame(element.0) {
                    fields.push(ReplyBox {
                        x: f.x,
                        y: f.y,
                        width: f.width,
                        height: f.height,
                    });
                }
                continue;
            }
            pending.extend(children(element.0));
        }
        pick(&fields, window_frame)
    }
}

#[cfg(all(target_os = "macos", test))]
impl platform::Survey {
    pub fn of(pid: i32) -> Self {
        platform::survey(pid)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{pick, Frame, ReplyBox, MAX_NODES};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Descendants,
        UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

    fn automation() -> Option<IUIAutomation> {
        unsafe {
            // Already initialised on this thread is fine: the result is ignored.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
        }
    }

    fn is_text_field(element: &IUIAutomationElement) -> bool {
        match unsafe { element.CurrentControlType() } {
            Ok(kind) => kind == UIA_EditControlTypeId || kind == UIA_DocumentControlTypeId,
            Err(_) => false,
        }
    }

    fn rect_to_box(rect: RECT) -> ReplyBox {
        ReplyBox {
            x: rect.left as f64,
            y: rect.top as f64,
            width: (rect.right - rect.left) as f64,
            height: (rect.bottom - rect.top) as f64,
        }
    }

    pub fn focus_is_editable(_front_app_pid: Option<i32>) -> Option<bool> {
        let automation = automation()?;
        let focused = unsafe { automation.GetFocusedElement() }.ok()?;
        Some(is_text_field(&focused))
    }

    pub fn find_reply_box(_front_app_pid: Option<i32>) -> Option<ReplyBox> {
        let automation = automation()?;
        let fields = unsafe {
            let hwnd = GetForegroundWindow();
            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect).ok()?;
            let window = rect_to_box(rect);
            let window = Frame {
                x: window.x,
                y: window.y,
                width: window.width,
                height: window.height,
            };
            let root = automation.ElementFromHandle(hwnd).ok()?;
            let all = automation.CreateTrueCondition().ok()?;
            let elements = root.FindAll(TreeScope_Descendants, &all).ok()?;
            let mut fields = Vec::new();
            for i in 0..elements.Length().ok()?.min(MAX_NODES as i32) {
                let Ok(element) = elements.GetElement(i) else {
                    continue;
                };
                if !is_text_field(&element) {
                    continue;
                }
                if element
                    .CurrentIsOffscreen()
                    .map(|b| b.as_bool())
                    .unwrap_or(true)
                {
                    continue;
                }
                if !element
                    .CurrentIsKeyboardFocusable()
                    .map(|b| b.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }
                if let Ok(rect) = element.CurrentBoundingRectangle() {
                    fields.push(rect_to_box(rect));
                }
            }
            (fields, window)
        };
        pick(&fields.0, fields.1)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::ReplyBox;

    pub fn focus_is_editable(_front_app_pid: Option<i32>) -> Option<bool> {
        None
    }

    pub fn find_reply_box(_front_app_pid: Option<i32>) -> Option<ReplyBox> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Frame = Frame {
        x: 0.0,
        y: 0.0,
        width: 1600.0,
        height: 900.0,
    };

    fn field(x: f64, y: f64, width: f64, height: f64) -> ReplyBox {
        ReplyBox {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn picks_the_lowest_wide_field_inside_the_window() {
        let fields = [
            // The browser's address bar.
            field(200.0, 10.0, 900.0, 28.0),
            // A search box in the chat list.
            field(20.0, 120.0, 100.0, 24.0),
            // The reply box above the Send button.
            field(420.0, 640.0, 600.0, 90.0),
        ];
        assert_eq!(pick(&fields, WINDOW), Some(fields[2]));
        assert_eq!(fields[2].center(), (720, 685));
    }

    /// Live check on this computer: `cargo test find_the_front_apps_reply_box
    /// -- --ignored --nocapture` with a chat page in front (the test binary
    /// needs the accessibility permission, or it prints None).
    #[test]
    #[ignore]
    fn find_the_front_apps_reply_box() {
        #[cfg(target_os = "macos")]
        let pid = {
            use objc2_app_kit::NSWorkspace;
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|app| app.processIdentifier())
        };
        #[cfg(not(target_os = "macos"))]
        let pid = None;
        // CHATELLE_AX_PID=<pid> looks at that app instead of the one in front.
        let pid = std::env::var("CHATELLE_AX_PID")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(pid);
        #[cfg(target_os = "macos")]
        println!("accessibility trusted: {}", platform::trusted());
        println!("front app pid: {pid:?}");
        #[cfg(target_os = "macos")]
        if let Some(pid) = pid {
            println!("survey: {:#?}", platform::Survey::of(pid));
        }
        println!("reply box: {:?}", find_reply_box(pid));
        println!("focus is editable: {:?}", focus_is_editable(pid));
    }

    #[test]
    fn a_document_view_or_an_offscreen_field_is_never_the_box() {
        let page = field(400.0, 60.0, 800.0, 800.0);
        let below_the_window = field(400.0, 950.0, 600.0, 60.0);
        assert_eq!(pick(&[page, below_the_window], WINDOW), None);
        assert_eq!(pick(&[], WINDOW), None);
    }
}
