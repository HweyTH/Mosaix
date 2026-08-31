//! Integration test: enumerate real windows on the current Windows desktop.
//!
//! This test runs on a live Windows machine and validates that the
//! enumeration pipeline returns sensible results.

#[cfg(windows)]
mod tests {
    use mosaix_domain::WindowLifecycle;
    use mosaix_platform_api::PlatformAdapter;
    use mosaix_platform_windows::WindowsPlatformAdapter;

    #[test]
    fn enumerate_returns_windows() {
        let adapter = WindowsPlatformAdapter::new();
        let windows = adapter
            .enumerate_windows()
            .expect("enumerate_windows should not fail");

        // On any interactive desktop session there should be at least one window
        // (the test runner itself, or a console host, etc.)
        assert!(
            !windows.is_empty(),
            "expected at least one manageable window on an interactive desktop"
        );

        println!(
            "\n=== Enumerated {} manageable windows ===\n",
            windows.len()
        );
        println!(
            "{:<8} {:<8} {:<30} {:<30} {:<12} {:<20} {:<10}",
            "HWND", "PID", "Title", "Class", "Role", "Bounds", "Lifecycle"
        );
        println!("{}", "-".repeat(120));

        for w in &windows {
            let bounds_str = format!(
                "{}x{} @{},{}",
                w.bounds.width, w.bounds.height, w.bounds.x, w.bounds.y
            );
            println!(
                "{:<8} {:<8} {:<30} {:<30} {:<12} {:<20} {:<10}",
                format!("{:#x}", w.id.0),
                w.process_id,
                truncate(&w.title, 28),
                truncate(w.native_class.as_deref().unwrap_or("(none)"), 28),
                format!("{:?}", w.role),
                bounds_str,
                format!("{:?}", w.lifecycle),
            );
        }
        println!();

        // Validate invariants on every returned window
        for w in &windows {
            // All returned windows should have positive-area bounds
            assert!(
                w.bounds.has_positive_area(),
                "window {:#x} '{}' has non-positive bounds: {:?}",
                w.id.0,
                w.title,
                w.bounds
            );

            // Lifecycle should not be Hidden (hidden windows are filtered out)
            assert_ne!(
                w.lifecycle,
                WindowLifecycle::Hidden,
                "window {:#x} '{}' should not have Hidden lifecycle",
                w.id.0,
                w.title,
            );

            // Should have either a title or a class name
            assert!(
                !w.title.is_empty() || w.native_class.is_some(),
                "window {:#x} has neither title nor class",
                w.id.0,
            );

            // Process ID should be nonzero
            assert_ne!(w.process_id, 0, "window {:#x} has zero process ID", w.id.0,);
        }
    }

    /// Truncate a string for display purposes.
    fn truncate(s: &str, max_len: usize) -> String {
        if s.len() <= max_len {
            s.to_string()
        } else {
            format!("{}…", &s[..max_len - 1])
        }
    }
}
