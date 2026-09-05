//! Live probe of the macOS recovery and parking surface against the real
//! windows on this machine. Read-only: it never moves a window.

use std::collections::BTreeMap;

use mosaix_platform_macos as mac;

fn main() {
    println!(
        "Accessibility trusted: {}",
        mac::accessibility::is_process_trusted()
    );

    let displays = mac::enumerate_displays().expect("displays enumerate");
    println!("displays: {}", displays.len());
    for d in &displays {
        println!("  {:?} {:?} primary={}", d.id, d.full_bounds, d.is_primary);
    }

    let (capability, site) = mac::parking_capability(&displays);
    println!("\nparking capability: {capability:?}");
    if let Some(site) = &site {
        println!("  site edge={:?} probe={:?}", site.edge, site.probe_rect());
        println!("  live re-verify: {}", mac::verify_parking_site(site));
    }

    // Walk the window server's on-screen list the same way foreground()
    // does, then put every id through the recovery surface.
    let mut resolved = 0usize;
    let mut ambiguous = 0usize;
    let mut unresolvable = 0usize;
    let mut probed = 0usize;
    let mut by_app: BTreeMap<String, usize> = BTreeMap::new();

    let ids = on_screen_window_ids();
    println!("\non-screen normal-layer windows: {}", ids.len());

    for id in &ids {
        let Some(info) = mac::window_server_info(*id) else {
            continue;
        };
        let evidence = mac::probe_handle(mac::WindowHandle(*id as isize));
        if evidence.is_some() {
            probed += 1;
        }
        let title = info.title.clone().unwrap_or_else(|| "<no title>".into());
        match mac::resolve_window(*id) {
            Ok(_) => {
                resolved += 1;
                println!("  ok        {id:>6}  pid {:<7} {title}", info.owner_pid);
            }
            Err(mac::MacosError::AmbiguousWindow { candidates, .. }) => {
                ambiguous += 1;
                *by_app.entry(title.clone()).or_default() += 1;
                println!(
                    "  AMBIGUOUS {id:>6}  pid {:<7} {candidates} candidates  {title}",
                    info.owner_pid
                );
            }
            Err(e) => {
                unresolvable += 1;
                println!(
                    "  unresolved{id:>6}  pid {:<7} {e}  {title}",
                    info.owner_pid
                );
            }
        }
    }

    println!("\n--- summary ---");
    println!("probed to a process instance: {probed}/{}", ids.len());
    println!("resolved to one AX window:    {resolved}");
    println!("ambiguous (refused):          {ambiguous}");
    println!("unresolvable (refused):       {unresolvable}");
    if !by_app.is_empty() {
        println!("ambiguous titles: {by_app:?}");
    }
}

fn on_screen_window_ids() -> Vec<u32> {
    // Reuse the adapter's own frontmost walk by asking for a wide range of
    // ids is not possible; instead probe the ids the window server hands
    // out for on-screen windows via the public list.
    mac::on_screen_window_ids()
}
