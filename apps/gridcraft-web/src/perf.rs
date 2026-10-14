//! Startup timings: steps marked as the app starts, logged once to the console as a report to
//! copy (devtools: "GridCraft startup").

use std::cell::{Cell, RefCell};

use wasm_bindgen::JsCast as _;

thread_local! {
    static MARKS: RefCell<Vec<(String, f64)>> = const { RefCell::new(Vec::new()) };
    static REPORTED: Cell<bool> = const { Cell::new(false) };
}

/// Milliseconds since the page started loading.
pub fn now() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
}

/// Notes that a startup step finished now. Ignored once the report is out.
pub fn mark(step: impl Into<String>) {
    if !REPORTED.get() {
        MARKS.with_borrow_mut(|m| m.push((step.into(), now())));
    }
}

/// Logs the steps marked so far, once.
pub fn report() {
    if REPORTED.replace(true) {
        return;
    }
    let mut steps: Vec<(String, f64)> = vec![];
    let performance = web_sys::window().and_then(|w| w.performance());
    if let Some(p) = &performance {
        if let Some(nav) = p.get_entries_by_type("navigation").get(0).dyn_ref::<web_sys::PerformanceNavigationTiming>() {
            steps.push(("page loaded (HTML)".into(), nav.response_end()));
            steps.push(("page parsed (DOMContentLoaded)".into(), nav.dom_content_loaded_event_end()));
        }
        for entry in p.get_entries_by_type("resource").iter() {
            let Some(r) = entry.dyn_ref::<web_sys::PerformanceResourceTiming>() else { continue };
            let name = r.name();
            let file = name.rsplit('/').next().unwrap_or_default();
            if file.ends_with(".wasm") || file.starts_with("gridcraft-web") {
                let size = r.decoded_body_size().max(r.encoded_body_size());
                steps.push((format!("fetched {file} ({:.1} MB, started {:.0})", size / 1e6, r.start_time()), r.response_end()));
            }
        }
    }
    steps.extend(MARKS.take());
    steps.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut out = String::from("GridCraft startup (ms since the page started loading; +ms since the step before)\n");
    let mut last = 0.0;
    for (step, at) in steps {
        out.push_str(&format!("{at:8.0} {:>+7.0}  {step}\n", at - last));
        last = at;
    }
    let agent = web_sys::window().and_then(|w| w.navigator().user_agent().ok()).unwrap_or_default();
    out.push_str(&format!("cores: {}  agent: {agent}", web_sys::window().map_or(0.0, |w| w.navigator().hardware_concurrency())));
    log::info!("{out}");
}
