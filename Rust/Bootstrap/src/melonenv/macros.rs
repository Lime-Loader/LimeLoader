#[macro_export]
macro_rules! debug_enabled {
    () => {{
        // A diagnostics build always logs debug!, otherwise it is release-gated behind the flag -
        // which is exactly why earlier release builds appeared to log nothing at all.
        if cfg!(feature = "diagnostics") {
            true
        } else if cfg!(debug_assertions) {
            true
        } else {
            let args: Vec<String> = std::env::args().collect();
            args.contains(&"--melonloader.debug".to_string())
        }
    }};
}

#[macro_export]
macro_rules! should_set_title {
    () => {{
        let args: Vec<String> = std::env::args().collect();
        !args.contains(&"--melonloader.consoledst".to_string())
    }};
}

#[macro_export]
macro_rules! console_on_top {
    () => {{
        let args: Vec<String> = std::env::args().collect();
        args.contains(&"--melonloader.consoleontop".to_string())
    }};
}

#[macro_export]
macro_rules! hide_console {
    () => {{
        let args: Vec<String> = std::env::args().collect();
        args.contains(&"--melonloader.hideconsole".to_string())
    }};
}