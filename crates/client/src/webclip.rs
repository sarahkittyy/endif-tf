//! Clipboard access that works on both targets, and the other small favours the page does for the
//! web build.
//!
//! Desktop uses Bevy's `Clipboard` (arboard). Browsers only allow clipboard access from inside a
//! user gesture, and `readText` is permission-gated (a prompt in Chrome, a "Paste" button in
//! Firefox 125+, nothing at all before that), so the web build goes through small helpers in
//! `web/index.html`: `endifCopy(text)` writes with a `execCommand("copy")` fallback, and pasted
//! text arrives through the browser's own `paste` event into a buffer the app polls with
//! `endifTakePasted()`. The page swallows Ctrl/Cmd+V before winit can `preventDefault()` it, since
//! that would cancel the paste; the game therefore never sees that key on the web, and
//! `endifRequestPaste()` (readText) is only for the on-screen paste button. `endifOpen(url)` opens
//! a link in a new tab (the download screen).
//!
//! Keeping the tab alive under game input (Ctrl+W, crouch while running forward, closes it):
//! `endifSetInMatch(bool)` arms a `beforeunload` prompt for the length of a match,
//! `endifSetFullscreenOnPlay(bool)` mirrors the fullscreen setting so the click that locks the
//! pointer also takes the page fullscreen, and `endifCanLockKeyboard()` says whether the browser
//! has the Keyboard Lock API that then makes Ctrl+W an ordinary key (Chromium only).

use bevy::clipboard::{Clipboard, ClipboardRead};

/// Copies text to the system clipboard. Returns false if the platform refused.
pub fn copy(clipboard: &mut Clipboard, text: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = clipboard;
        js::copy(text).is_ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        clipboard.set_text(text.to_string()).is_ok()
    }
}

/// Starts a clipboard read. Desktop returns the read handle to poll; the web build asks the page to
/// fetch the text into its buffer instead.
pub fn request_paste(clipboard: &mut Clipboard) -> Option<ClipboardRead> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = clipboard;
        let _ = js::request_paste();
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(clipboard.fetch_text())
    }
}

/// Text the page received from a paste gesture since the last call (web only).
pub fn take_pasted() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        js::take_pasted().ok().flatten().filter(|s| !s.is_empty())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

/// Opens a URL in a new tab (web only; the desktop build has nothing to open it with and no
/// screen that asks). Browsers treat the click that reached the game as recent user activation, so
/// the popup is allowed; if it is blocked anyway the page navigates to the URL instead, which for a
/// download (`Content-Disposition: attachment`) leaves the game where it is.
pub fn open_url(url: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Err(e) = js::open_url(url) {
            log::warn!("could not open {url}: {e:?}");
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        log::warn!("open_url({url}) is only available in the web build");
    }
}

/// Tells the page whether a match is running, so closing the tab asks first (web only).
pub fn set_in_match(on: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = js::set_in_match(on);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = on;
    }
}

/// Mirrors the "fullscreen on play" setting to the page (web only).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn set_fullscreen_on_play(on: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = js::set_fullscreen_on_play(on);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = on;
    }
}

/// Whether the browser can hand browser shortcuts such as Ctrl+W to the page while fullscreen.
/// Always false on desktop, where the question does not arise.
pub fn can_lock_keyboard() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        js::can_lock_keyboard().unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

#[cfg(target_arch = "wasm32")]
mod js {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifCopy)]
        pub fn copy(text: &str) -> Result<(), JsValue>;
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifRequestPaste)]
        pub fn request_paste() -> Result<(), JsValue>;
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifTakePasted)]
        pub fn take_pasted() -> Result<Option<String>, JsValue>;
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifOpen)]
        pub fn open_url(url: &str) -> Result<(), JsValue>;
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifSetInMatch)]
        pub fn set_in_match(on: bool) -> Result<(), JsValue>;
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifSetFullscreenOnPlay)]
        pub fn set_fullscreen_on_play(on: bool) -> Result<(), JsValue>;
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifCanLockKeyboard)]
        pub fn can_lock_keyboard() -> Result<bool, JsValue>;
    }
}
