use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, msg_send_id, ClassType, MainThreadOnly};
use objc2_app_kit::{NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuDelegate, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

use crate::config::load_config;
use crate::data_collector::{calc_streak_fast, collect_day_metrics};
use crate::server::PORT;

// ── Ring definitions (radius, r, g, b) — matches style.css ───────────────────

const RING_DEFS: [(f64, f64, f64, f64); 3] = [
    (25.0, 1.000, 0.216, 0.373), // outer — red #FF375F (consume)
    (17.5, 0.188, 0.820, 0.345), // middle — green #30D158 (focus)
    (10.0, 0.039, 0.518, 1.000), // inner — blue #0A84FF (action)
];
const TRACK_W: f64 = 5.5;
const VIEW_H: f64 = 68.0;
const VIEW_W: f64 = 300.0;

// ── Global ring percentages ───────────────────────────────────────────────────

static RING_PCTS: std::sync::RwLock<[f64; 3]> = std::sync::RwLock::new([0.0, 0.0, 0.0]);

// ── VibeRingsView — custom NSView for ring drawing ────────────────────────────

define_class!(
    #[unsafe(super(NSView))]
    #[name = "VibeRingsView"]
    #[ivars = ()]
    pub struct VibeRingsView;

    impl VibeRingsView {
        #[unsafe(method(drawRect:))]
        unsafe fn draw_rect(&self, _rect: NSRect) {
            let pcts = match RING_PCTS.read() {
                Ok(p) => *p,
                Err(_) => [0.0, 0.0, 0.0],
            };

            let bounds: NSRect = unsafe { msg_send![self, bounds] };
            let cx = bounds.size.width / 2.0;
            let cy = bounds.size.height / 2.0;

            // Clear background
            let clear: &AnyObject = unsafe { msg_send![objc2::class!(NSColor), clearColor] };
            let _: () = unsafe { msg_send![clear, set] };

            for ((radius, r, g, b), pct) in RING_DEFS.iter().zip(pcts.iter()) {
                let capped = pct.min(1.0);

                // Draw track (dim background ring)
                unsafe {
                    let dim: &AnyObject = msg_send![
                        objc2::class!(NSColor),
                        colorWithCalibratedRed: r * 0.18
                        green: g * 0.18
                        blue: b * 0.18
                        alpha: 1.0
                    ];
                    let _: () = msg_send![dim, set];
                    let track: &AnyObject = msg_send![objc2::class!(NSBezierPath), bezierPath];
                    let _: () = msg_send![track, setLineWidth: TRACK_W];
                    let _: () = msg_send![
                        track,
                        appendBezierPathWithArcWithCenter: NSPoint::new(cx, cy)
                        radius: *radius
                        startAngle: 0.0
                        endAngle: 360.0
                    ];
                    let _: () = msg_send![track, stroke];
                }

                if *pct <= 0.001 {
                    continue;
                }

                // Draw progress arc
                unsafe {
                    let color: &AnyObject = msg_send![
                        objc2::class!(NSColor),
                        colorWithCalibratedRed: *r
                        green: *g
                        blue: *b
                        alpha: 1.0
                    ];
                    let _: () = msg_send![color, set];
                    let arc: &AnyObject = msg_send![objc2::class!(NSBezierPath), bezierPath];
                    let _: () = msg_send![arc, setLineWidth: TRACK_W];
                    let _: () = msg_send![arc, setLineCapStyle: 1]; // NSRoundLineCapStyle

                    if capped >= 1.0 {
                        let _: () = msg_send![
                            arc,
                            appendBezierPathWithArcWithCenter: NSPoint::new(cx, cy)
                            radius: *radius
                            startAngle: 0.0
                            endAngle: 360.0
                        ];
                    } else {
                        let _: () = msg_send![
                            arc,
                            appendBezierPathWithArcWithCenter: NSPoint::new(cx, cy)
                            radius: *radius
                            startAngle: 90.0
                            endAngle: 90.0 - capped * 360.0
                            clockwise: true
                        ];
                    }
                    let _: () = msg_send![arc, stroke];
                }
            }
        }
    }
);

impl VibeRingsView {
    fn update_pcts(&self, pcts: [f64; 3]) {
        if let Ok(mut guard) = RING_PCTS.write() {
            *guard = pcts;
        }
        unsafe {
            let _: () = msg_send![self, setNeedsDisplay: true];
        }
    }
}

// ── Formatters ────────────────────────────────────────────────────────────────

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let v = n as f64 / 1_000_000.0;
        if v % 1.0 == 0.0 {
            format!("{}M", v as u64)
        } else {
            format!("{v:.1}M")
        }
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

fn fmt_pct(pct: f64) -> String {
    format!("{}%", (pct * 100.0).round() as u32)
}

// ── MenuBar app state (main-thread only) ──────────────────────────────────────

struct MenuBarUI {
    rings_view: Retained<VibeRingsView>,
    item_tokens: Retained<AnyObject>,
    item_focus: Retained<AnyObject>,
    item_tools: Retained<AnyObject>,
    item_streak: Retained<AnyObject>,
    item_open: Retained<AnyObject>,
}

thread_local! {
    static MENU_UI: RefCell<Option<Rc<MenuBarUI>>> = RefCell::new(None);
}

// ── MenuActionTarget — handles menu item clicks and menu opening ──────────────

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VibeMenuTarget"]
    #[ivars = ()]
    #[thread_kind = MainThreadOnly]
    pub struct MenuActionTarget;

    impl MenuActionTarget {
        #[unsafe(method(openDashboard:))]
        unsafe fn open_dashboard(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("open")
                .arg(format!("http://localhost:{PORT}"))
                .spawn();
        }

        #[unsafe(method(openDetailTokens:))]
        unsafe fn open_detail_tokens(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("open")
                .arg(format!("http://localhost:{PORT}/#detail=tokens"))
                .spawn();
        }

        #[unsafe(method(openDetailFocus:))]
        unsafe fn open_detail_focus(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("open")
                .arg(format!("http://localhost:{PORT}/#detail=focus"))
                .spawn();
        }

        #[unsafe(method(openDetailTools:))]
        unsafe fn open_detail_tools(&self, _sender: Option<&AnyObject>) {
            let _ = Command::new("open")
                .arg(format!("http://localhost:{PORT}/#detail=tools"))
                .spawn();
        }

        #[unsafe(method(quitApp:))]
        unsafe fn quit_app(&self, _sender: Option<&AnyObject>) {
            std::process::exit(0);
        }

        // NSMenuDelegate method - called when menu is about to open
        #[unsafe(method(menuWillOpen:))]
        unsafe fn menu_will_open(&self, _menu: Option<&NSMenu>) {
            refresh_stats();
        }
    }
);

// Implement required protocols
unsafe impl NSObjectProtocol for MenuActionTarget {}
unsafe impl NSMenuDelegate for MenuActionTarget {}

// ── Refresh stats (called on main thread) ─────────────────────────────────────

fn refresh_stats() {
    MENU_UI.with(|cell| {
        let ui = match cell.borrow().as_ref() {
            Some(u) => u.clone(),
            None => return,
        };

        let goals = load_config();
        let zh = goals.lang == "zh";
        let today = chrono::Local::now().date_naive();
        // 只获取当天数据，不再扫描7天历史
        let metrics = collect_day_metrics(today, &goals);
        // 使用缓存计算 streak
        let streak = calc_streak_fast(&metrics, &goals);

        let tp = metrics.token_pct.unwrap_or(0.0);
        let fp = metrics.focus_pct.unwrap_or(0.0);
        let ap = metrics.tool_pct.unwrap_or(0.0);

        // Update ring view
        ui.rings_view.update_pcts([tp, fp, ap]);

        // Format metric strings
        let tok_str = fmt_tokens(metrics.tokens);
        let tok_goal = fmt_tokens(goals.tokens);
        let foc_str = format!("{}", metrics.focus_min.round() as u64);
        let tol_str = metrics.tool_calls.to_string();

        let (tokens_title, focus_title, tools_title, streak_title, open_title) = if zh {
            (
                format!("消耗   {tok_str} / {tok_goal}  ({})", fmt_pct(tp)),
                format!("专注   {foc_str} / {} 分钟  ({})", goals.focus_min, fmt_pct(fp)),
                format!("行动   {tol_str} / {} 次  ({})", goals.tool_calls, fmt_pct(ap)),
                format!("🔥  连续达标 {streak} 天"),
                "打开看板 ↗".to_string(),
            )
        } else {
            (
                format!("Consume   {tok_str} / {tok_goal}  ({})", fmt_pct(tp)),
                format!("Focus   {foc_str} / {} min  ({})", goals.focus_min, fmt_pct(fp)),
                format!("Action   {tol_str} / {} calls  ({})", goals.tool_calls, fmt_pct(ap)),
                format!("🔥  {streak}-day streak"),
                "Open Dashboard ↗".to_string(),
            )
        };

        unsafe {
            let _: () = msg_send![&*ui.item_tokens, setTitle: &*NSString::from_str(&tokens_title)];
            let _: () = msg_send![&*ui.item_focus, setTitle: &*NSString::from_str(&focus_title)];
            let _: () = msg_send![&*ui.item_tools, setTitle: &*NSString::from_str(&tools_title)];
            let _: () = msg_send![&*ui.item_streak, setTitle: &*NSString::from_str(&streak_title)];
            let _: () = msg_send![&*ui.item_open, setTitle: &*NSString::from_str(&open_title)];
        }
    });
}

// ── Create ring icon image (template → macOS renders white automatically) ─────

unsafe fn create_ring_icon() -> Retained<NSImage> {
    let size = NSSize::new(20.0, 20.0);
    // Use raw pointers to avoid Retained<NSImage>: Encode issue
    let alloc_ptr: *mut AnyObject = msg_send![objc2::class!(NSImage), alloc];
    let init_ptr: *mut AnyObject = msg_send![alloc_ptr, initWithSize: size];
    let image: Retained<NSImage> = Retained::from_raw(init_ptr as *mut NSImage).unwrap();
    let _: () = msg_send![&image, lockFocus];

    let cx = 10.0;
    let cy = 10.0;
    let line_width = 1.8;
    let radii: [f64; 3] = [8.5, 5.8, 3.1];

    // Draw in black — template mode makes macOS render it in menu-bar color (white)
    let black: &AnyObject = msg_send![objc2::class!(NSColor), blackColor];
    let _: () = msg_send![black, set];

    for radius in &radii {
        let path: &AnyObject = msg_send![objc2::class!(NSBezierPath), bezierPath];
        let _: () = msg_send![path, setLineWidth: line_width];
        let _: () = msg_send![
            path,
            appendBezierPathWithArcWithCenter: NSPoint::new(cx, cy)
            radius: *radius
            startAngle: 0.0
            endAngle: 360.0
        ];
        let _: () = msg_send![path, stroke];
    }

    let _: () = msg_send![&image, unlockFocus];
    let _: () = msg_send![&image, setTemplate: true];

    image
}

// ── Setup and run ─────────────────────────────────────────────────────────────

pub fn run_menubar() {
    unsafe {
        // NSApplication
        let app: Retained<AnyObject> = msg_send_id![objc2::class!(NSApplication), sharedApplication];
        let _: () = msg_send![&app, setActivationPolicy: NSApplicationActivationPolicy::Accessory];

        // Create delegate — use T::class() to ensure class registration
        let delegate: Retained<MenuActionTarget> = msg_send_id![MenuActionTarget::class(), new];

        // Status bar
        let status_bar: Retained<AnyObject> = msg_send_id![objc2::class!(NSStatusBar), systemStatusBar];
        let status_item: Retained<AnyObject> = msg_send_id![&status_bar, statusItemWithLength: -1.0];

        let button: Retained<AnyObject> = msg_send_id![&status_item, button];
        let icon = create_ring_icon();
        let _: () = msg_send![&button, setImage: &*icon];

        // Menu
        let menu: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenu), new];

        // Header
        let item_header: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_header, setTitle: &*NSString::from_str("VIBE CODING RINGS")];
        let _: () = msg_send![&item_header, setEnabled: false];
        let _: () = msg_send![&menu, addItem: &*item_header];

        // Separator
        let sep: Retained<AnyObject> = msg_send![objc2::class!(NSMenuItem), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep];

        // Rings view
        let item_rings: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_rings, setEnabled: false];
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(VIEW_W, VIEW_H));
        let rings_alloc: *mut AnyObject = msg_send![VibeRingsView::class(), alloc];
        let rings_init: *mut AnyObject = msg_send![rings_alloc, initWithFrame: frame];
        let rings_view: Retained<VibeRingsView> = Retained::from_raw(rings_init as *mut VibeRingsView).unwrap();
        let _: () = msg_send![&item_rings, setView: &*rings_view];
        let _: () = msg_send![&menu, addItem: &*item_rings];

        // Separator
        let sep2: Retained<AnyObject> = msg_send![objc2::class!(NSMenuItem), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep2];

        // Metric items
        let item_tokens: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_tokens, setTitle: &*NSString::from_str("—")];
        let _: () = msg_send![&item_tokens, setTarget: &*delegate];
        let _: () = msg_send![&item_tokens, setAction: objc2::sel!(openDetailTokens:)];
        let _: () = msg_send![&menu, addItem: &*item_tokens];

        let item_focus: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_focus, setTitle: &*NSString::from_str("—")];
        let _: () = msg_send![&item_focus, setTarget: &*delegate];
        let _: () = msg_send![&item_focus, setAction: objc2::sel!(openDetailFocus:)];
        let _: () = msg_send![&menu, addItem: &*item_focus];

        let item_tools: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_tools, setTitle: &*NSString::from_str("—")];
        let _: () = msg_send![&item_tools, setTarget: &*delegate];
        let _: () = msg_send![&item_tools, setAction: objc2::sel!(openDetailTools:)];
        let _: () = msg_send![&menu, addItem: &*item_tools];

        // Separator
        let sep3: Retained<AnyObject> = msg_send![objc2::class!(NSMenuItem), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep3];

        // Streak
        let item_streak: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_streak, setTitle: &*NSString::from_str("—")];
        let _: () = msg_send![&item_streak, setEnabled: false];
        let _: () = msg_send![&menu, addItem: &*item_streak];

        // Separator
        let sep4: Retained<AnyObject> = msg_send![objc2::class!(NSMenuItem), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep4];

        // Open dashboard
        let item_open: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_open, setTitle: &*NSString::from_str("Open Dashboard ↗")];
        let _: () = msg_send![&item_open, setTarget: &*delegate];
        let _: () = msg_send![&item_open, setAction: objc2::sel!(openDashboard:)];
        let _: () = msg_send![&menu, addItem: &*item_open];

        // Separator
        let sep5: Retained<AnyObject> = msg_send![objc2::class!(NSMenuItem), separatorItem];
        let _: () = msg_send![&menu, addItem: &*sep5];

        // Quit
        let item_quit: Retained<AnyObject> = msg_send_id![objc2::class!(NSMenuItem), new];
        let _: () = msg_send![&item_quit, setTitle: &*NSString::from_str("Quit")];
        let _: () = msg_send![&item_quit, setTarget: &*delegate];
        let _: () = msg_send![&item_quit, setAction: objc2::sel!(quitApp:)];
        let _: () = msg_send![&menu, addItem: &*item_quit];

        // Set menu on status item
        let _: () = msg_send![&status_item, setMenu: &*menu];

        // Set menu delegate to receive menuWillOpen events
        let delegate_proto: Retained<ProtocolObject<dyn NSMenuDelegate>> = ProtocolObject::from_retained(delegate.clone());
        let _: () = msg_send![&menu, setDelegate: Some(&*delegate_proto)];

        // Keep references alive
        let ui = Rc::new(MenuBarUI {
            rings_view,
            item_tokens,
            item_focus,
            item_tools,
            item_streak,
            item_open,
        });

        MENU_UI.with(|cell| {
            *cell.borrow_mut() = Some(ui);
        });

        // Run the app
        let _: () = msg_send![&app, run];
    }
}
