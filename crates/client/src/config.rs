//! Client configuration: signaling server and account API addresses, room code from the URL, dev
//! flags. On desktop a `.env` file in the working directory is loaded first (see `.env.example`).

use bevy::prelude::*;

/// Fallback when nothing else names a signaling server: a local dev server.
pub const DEFAULT_SIGNALING: &str = "ws://127.0.0.1:3536";
/// Signaling server baked in at build time: `ENDIF_SIGNALING=wss://signal.host cargo build ...`
/// (also picked up by `build-web.sh`, which exports `.env`). Runtime settings still override it.
const BUILT_IN_SIGNALING: Option<&str> = option_env!("ENDIF_SIGNALING");
/// Account API base baked in at build time (`ENDIF_API`); empty means "derive from the signaling URL".
const BUILT_IN_API: Option<&str> = option_env!("ENDIF_API");
/// ICE servers baked in at build time (`ENDIF_ICE`), same format as the runtime setting.
const BUILT_IN_ICE: Option<&str> = option_env!("ENDIF_ICE");
/// ICE servers used when nothing else is configured: Google's public STUN plus the endif.tf TURN
/// relay (coturn on sarahvps2 behind `endif.tf`, see the README), which gets two browsers on one LAN or
/// peers behind strict NATs connected. The credential is public by nature (it ships in the web
/// build); the relay is quota-limited and refuses to relay into private networks.
pub const DEFAULT_ICE: &str = "stun:stun.l.google.com:19302,stun:stun1.l.google.com:19302,turn:endif.tf:3478|endif|2cedf86bb9ef5599bff1f43cd3c54b76";

/// Port the server binds by default; used when the web build derives the address from the page.
#[cfg(target_arch = "wasm32")]
const DEFAULT_SIGNALING_PORT: u16 = 3536;

/// The signaling server to use before any runtime override (`--server`, `ENDIF_SIGNALING` env,
/// `?server=`): the build-time value, then (web only) `window.ENDIF_SIGNALING` from the page, then
/// the page's own host on the default port, then the local dev server.
pub fn default_signaling() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(url) = page_signaling() {
            return url;
        }
    }
    if let Some(url) = BUILT_IN_SIGNALING.map(str::trim).filter(|u| !u.is_empty()) {
        return url.to_string();
    }
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(url) = derived_signaling() {
            return url;
        }
    }
    DEFAULT_SIGNALING.to_string()
}

/// `window.ENDIF_SIGNALING`, set by `build-web.sh` into `index.html` (empty means unset).
#[cfg(target_arch = "wasm32")]
fn page_signaling() -> Option<String> {
    let window = web_sys::window()?;
    let v = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("ENDIF_SIGNALING")).ok()?;
    let s = v.as_string()?.trim().to_string();
    (!s.is_empty() && !s.starts_with("__")).then_some(s)
}

/// `ws(s)://<page host>:3536` when the page is served over http(s).
#[cfg(target_arch = "wasm32")]
fn derived_signaling() -> Option<String> {
    let loc = web_sys::window()?.location();
    let host = loc.hostname().ok()?;
    if host.is_empty() {
        return None;
    }
    let scheme = if loc.protocol().ok()? == "https:" { "wss" } else { "ws" };
    Some(format!("{scheme}://{host}:{DEFAULT_SIGNALING_PORT}"))
}
/// Every capital letter plus 2-9: the menu font keeps I/O distinct, and leaving 0 and 1 out means
/// a round or vertical glyph can only be O or I.
const ROOM_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ23456789";
pub const ROOM_CODE_LEN: usize = 6;

/// `http(s)://host:port` of a `ws(s)://` URL.
fn http_base(ws_url: &str) -> String {
    ws_url.trim_end_matches('/').replacen("wss://", "https://", 1).replacen("ws://", "http://", 1)
}

#[derive(Resource, Clone, Debug)]
pub struct ClientConfig {
    /// Base URL of the signaling server, e.g. `ws://host:3536`.
    pub signaling_url: String,
    /// Base URL of the account API when it is not the signaling server's host (`ENDIF_API`).
    pub api_override: Option<String>,
    /// ICE servers, comma separated: `stun:host:port` or `turn:host:port|username|credential`.
    pub ice: String,
    /// Room code supplied on the command line / URL (`?room=CODE`), if any.
    pub initial_room: Option<String>,
    /// `?qp` / `--quick`: join the quick play queue as soon as the menu is up (the invite link
    /// from the quick play screen).
    pub initial_quick: bool,
    /// `--name` / `?name=`: anonymous display name for this run (not saved).
    pub player_name: Option<String>,
    /// Dev: start an offline practice match immediately.
    pub auto_practice: bool,
    /// Dev: write a screenshot to this path a few seconds after the match starts.
    pub screenshot: Option<String>,
    /// Dev: exit after this many seconds.
    pub quit_after: Option<f64>,
}

impl ClientConfig {
    pub fn load() -> Self {
        // `.env` from the working directory; real environment variables win over it.
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(path) = dotenvy::dotenv() {
            info!("loaded {}", path.display());
        }

        // Build-time values are trimmed: a `.env` with CRLF line endings once compiled a lone
        // carriage return into ENDIF_ICE, which parsed to zero ICE servers.
        let built_in = |v: Option<&'static str>| v.map(str::trim).filter(|s| !s.is_empty());
        let mut cfg = ClientConfig {
            signaling_url: default_signaling(),
            api_override: built_in(BUILT_IN_API).map(str::to_string),
            ice: built_in(BUILT_IN_ICE).unwrap_or(DEFAULT_ICE).to_string(),
            initial_room: None,
            initial_quick: false,
            player_name: None,
            auto_practice: false,
            screenshot: None,
            quit_after: None,
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            if let Some(url) = env("ENDIF_SIGNALING") {
                cfg.signaling_url = url;
            }
            if let Some(api) = env("ENDIF_API") {
                cfg.api_override = Some(api);
            }
            if let Some(ice) = env("ENDIF_ICE") {
                cfg.ice = ice;
            }
            let mut args = std::env::args().skip(1);
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--room" => cfg.initial_room = args.next().map(normalize_room_code),
                    "--quick" => cfg.initial_quick = true,
                    "--server" => {
                        if let Some(u) = args.next() {
                            cfg.signaling_url = u;
                        }
                    }
                    "--api" => cfg.api_override = args.next(),
                    "--name" => cfg.player_name = args.next(),
                    "--ice" => {
                        if let Some(i) = args.next() {
                            cfg.ice = i;
                        }
                    }
                    "--practice" => cfg.auto_practice = true,
                    "--screenshot" => cfg.screenshot = args.next(),
                    "--quit-after" => cfg.quit_after = args.next().and_then(|v| v.parse().ok()),
                    _ => {}
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            if let Some(search) = web_sys::window().and_then(|w| w.location().search().ok()) {
                for (k, v) in parse_query(&search) {
                    match k.as_str() {
                        "room" => cfg.initial_room = Some(normalize_room_code(v)),
                        "qp" => cfg.initial_quick = true,
                        "server" => cfg.signaling_url = v,
                        "api" => cfg.api_override = Some(v),
                        "ice" => cfg.ice = v,
                        "name" => cfg.player_name = Some(v),
                        _ => {}
                    }
                }
            }
        }

        cfg
    }

    /// The ICE server list for WebRTC: every URL, plus the credentials of the first TURN entry
    /// (matchbox takes one server config with several URLs).
    pub fn ice_servers(&self) -> matchbox_socket::RtcIceServerConfig {
        let mut cfg = Self::parse_ice(&self.ice);
        if cfg.urls.is_empty() {
            // Browsers reject an ICE server entry without URLs (Firefox throws, and matchbox
            // unwraps that), and a peer connection without STUN/TURN only ever works on one LAN.
            warn!("ICE server list {:?} holds no servers; using the built-in default", self.ice);
            cfg = Self::parse_ice(DEFAULT_ICE);
        }
        cfg
    }

    fn parse_ice(list: &str) -> matchbox_socket::RtcIceServerConfig {
        let mut cfg = matchbox_socket::RtcIceServerConfig { urls: Vec::new(), username: None, credential: None };
        for entry in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let mut parts = entry.split('|');
            let url = parts.next().unwrap_or_default().to_string();
            if let (Some(user), Some(pass)) = (parts.next(), parts.next())
                && cfg.username.is_none()
            {
                cfg.username = Some(user.to_string());
                cfg.credential = Some(pass.to_string());
            }
            cfg.urls.push(url);
        }
        cfg
    }

    /// Full signaling URL for a room, carrying this build's protocol identity for the server.
    pub fn room_url(&self, code: &str) -> String {
        format!("{}/endif-{}?v={}", self.signaling_url.trim_end_matches('/'), code, endif_sim::protocol_id())
    }

    /// The presence socket (`/presence` on the signaling server): open for as long as the app
    /// runs, so the server can count who is online.
    pub fn presence_url(&self) -> String {
        format!("{}/presence?v={}", self.signaling_url.trim_end_matches('/'), endif_sim::protocol_id())
    }

    /// `http(s)://host:port` of the account API: `ENDIF_API`, or the signaling server over HTTP.
    pub fn api_url(&self) -> String {
        match &self.api_override {
            Some(api) => api.trim_end_matches('/').to_string(),
            None => http_base(&self.signaling_url),
        }
    }

    /// `http(s)://.../version` of the signaling server (the web build fetches it to detect stale
    /// cached builds, since a browser websocket reports no HTTP status on refusal).
    pub fn version_url(&self) -> String {
        format!("{}/version", http_base(&self.signaling_url))
    }

    /// `http(s)://.../build` of the signaling server: the commit it was built from, compared to
    /// `endif_sim::BUILD_ID` to notice a newer build whose protocol still matches.
    pub fn build_url(&self) -> String {
        format!("{}/build", http_base(&self.signaling_url))
    }

    /// The desktop package for this platform, served by nginx next to the API
    /// (`/download/<platform>`, see `deploy/nginx/endif.tf.conf`). Desktop only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn download_url(&self) -> String {
        format!("{}/download/{}", http_base(&self.signaling_url), Self::PLATFORM)
    }

    /// The build id of the package at [`download_url`](Self::download_url), a text file the
    /// `downloads` job publishes next to it (`/download/<platform>.version`). The packages go up
    /// after the server restarts; until this matches the server's build the updater would only
    /// fetch the old package. Desktop only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn package_version_url(&self) -> String {
        format!("{}/download/{}.version", http_base(&self.signaling_url), Self::PLATFORM)
    }

    #[cfg(not(target_arch = "wasm32"))]
    const PLATFORM: &str = if cfg!(windows) { "windows" } else { "linux" };

    /// A shareable link (web builds) or the bare code (desktop builds).
    pub fn join_link(&self, code: &str) -> String {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(link) = self.page_link(&format!("room={code}")) {
                return link;
            }
        }
        code.to_string()
    }

    /// A link that puts whoever opens it straight into the quick play queue (`?qp`). The web
    /// build points at its own page; the desktop build at the site the server runs on (the site
    /// and the server share an origin on endif.tf).
    pub fn quick_play_link(&self) -> String {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(link) = self.page_link("qp") {
                return link;
            }
        }
        format!("{}/?qp", http_base(&self.signaling_url))
    }

    /// Web: the page's own address with `query` and, when the server is not the page's default
    /// one, `server=`, so the link opens against the same server.
    #[cfg(target_arch = "wasm32")]
    fn page_link(&self, query: &str) -> Option<String> {
        let loc = web_sys::window()?.location();
        let origin = loc.origin().unwrap_or_default();
        let path = loc.pathname().unwrap_or_default();
        let server = if self.signaling_url == default_signaling() { String::new() } else { format!("&server={}", self.signaling_url) };
        Some(format!("{origin}{path}?{query}{server}"))
    }
}

/// Web: drops `room=` and `qp` from the address bar once they have been acted on, so a reload
/// (or the address copied again later) does not silently rejoin a room whose host has long left,
/// or queue up again. Every other parameter (`server=`, `name=`...) stays. No-op on desktop.
pub fn forget_join_in_url() {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else { return };
        let loc = window.location();
        let (Ok(search), Ok(path)) = (loc.search(), loc.pathname()) else { return };
        let all: Vec<&str> = search.trim_start_matches('?').split('&').filter(|kv| !kv.is_empty()).collect();
        let rest: Vec<&str> = all.iter().copied().filter(|kv| !matches!(kv.split('=').next().unwrap_or_default(), "room" | "qp")).collect();
        if rest.len() == all.len() {
            return;
        }
        let url = if rest.is_empty() { path } else { format!("{path}?{}", rest.join("&")) };
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn parse_query(search: &str) -> Vec<(String, String)> {
    search
        .trim_start_matches('?')
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = it.next().unwrap_or("").to_string();
            let v = it.next().unwrap_or("").replace("%3A", ":").replace("%2F", "/");
            (k, v)
        })
        .collect()
}

/// Uppercase and drop characters that are not in the room alphabet (codes never contain 0 or 1).
pub fn normalize_room_code(s: impl AsRef<str>) -> String {
    s.as_ref()
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.is_ascii() && ROOM_ALPHABET.contains(&(*c as u8)))
        .take(ROOM_CODE_LEN)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_list_parses_stun_and_turn_entries() {
        let mut cfg = ClientConfig::load();
        cfg.ice = "stun:a:1, turn:b:3478|user|pass ,stun:c:2".to_string();
        let ice = cfg.ice_servers();
        assert_eq!(ice.urls, vec!["stun:a:1", "turn:b:3478", "stun:c:2"]);
        assert_eq!(ice.username.as_deref(), Some("user"));
        assert_eq!(ice.credential.as_deref(), Some("pass"));
    }

    #[test]
    fn api_follows_the_signaling_server_unless_overridden() {
        let mut cfg = ClientConfig::load();
        cfg.api_override = None;
        cfg.signaling_url = "wss://signal.example.org/".to_string();
        assert_eq!(cfg.api_url(), "https://signal.example.org");
        assert_eq!(cfg.version_url(), "https://signal.example.org/version");
        assert_eq!(cfg.build_url(), "https://signal.example.org/build");
        cfg.api_override = Some("https://api.example.org/".to_string());
        assert_eq!(cfg.api_url(), "https://api.example.org");
    }

    #[test]
    fn desktop_quick_play_link_points_at_the_site() {
        let mut cfg = ClientConfig::load();
        cfg.signaling_url = "wss://endif.tf".to_string();
        assert_eq!(cfg.quick_play_link(), "https://endif.tf/?qp");
        assert_eq!(cfg.presence_url(), format!("wss://endif.tf/presence?v={}", endif_sim::protocol_id()));
    }

    #[test]
    fn pasted_links_and_codes_both_yield_the_code() {
        assert_eq!(code_from_text("https://endif.tf/?room=ABCDEF&server=wss://x"), "ABCDEF");
        assert_eq!(code_from_text("  abcdef\n"), "ABCDEF");
        assert_eq!(code_from_text("room=lkjqwe"), "LKJQWE");
    }

    #[test]
    fn normalizer_keeps_valid_letters_verbatim() {
        assert_eq!(normalize_room_code("lkjq"), "LKJQ");
        assert_eq!(normalize_room_code("a-b c1o0"), "ABCO");
        assert_eq!(normalize_room_code("il1"), "IL");
        assert_eq!(normalize_room_code("ABCDEFGH"), "ABCDEF");
    }
}

/// Room code from pasted text: a bare code, or an invite link (`...?room=CODE...`).
pub fn code_from_text(s: &str) -> String {
    let s = s.trim();
    match s.find("room=") {
        Some(i) => normalize_room_code(&s[i + 5..]),
        None => normalize_room_code(s),
    }
}

pub fn generate_room_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..ROOM_CODE_LEN)
        .map(|_| ROOM_ALPHABET[rng.gen_range(0..ROOM_ALPHABET.len())] as char)
        .collect()
}

/// Deterministic seed shared by both peers: FNV-1a of the room code.
pub fn seed_from_room(code: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in code.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
