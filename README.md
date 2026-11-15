# Alouette

A modern internet suite for the terminal, in the spirit of
[Minuet](https://en.wikipedia.org/wiki/Minnesota_Internet_Users_Essential_Tool), the University
of Minnesota's 1990s DOS suite. One small binary for macOS, Linux and Windows gives you mail,
news and feeds, Gopher and Gemini, search and lookups, file transfer, SSH and Telnet, a web reader,
network tools, contacts, a calendar, chat, the fediverse and an optional AI assistant, all in one
window manager.

> **Status: milestone M4 (News and social).** The shell, the network layer and nine modules
> work today: **Burrow** (Gopher, Gemini, Finger), **Inquire** (one box that asks DNS, RDAP, WHOIS,
> BGP, certificate logs, WebFinger, OpenPGP WKD, DICT, Veronica-2 and Gemini search at once),
> **NetTools** (ping, traceroute, DNS, ports, TLS inspection, mDNS), **Terminal** (local shells,
> SSH, Telnet, Mosh), **Files** (two panes over local disks, SFTP, FTP/FTPS, WebDAV and S3),
> **Mail** (IMAP and SMTP, JMAP, Gmail and Microsoft 365), **News** (Usenet over NNTP),
> **Feeds** (RSS, Atom and JSON Feed) and **Social** (Mastodon and compatible servers). The rest arrive milestone by milestone; see the plan below.

## Try it

```sh
cargo run --release
cargo run --release -- --layout tiled about:tasks demo:jobs
cargo run --release -- --dump-screen 100x30      # render one frame as text, no terminal needed
```

Open **Modules > Demo > Background jobs** (or type `demo:jobs` after pressing F3) to start fake
downloads and watch them in the Tasks window, where you can cancel and retry them.

## Modules so far

| Module | Try | Does |
| --- | --- | --- |
| Burrow | `gopher://gopher.floodgap.com/`, `gemini://geminiprotocol.net/`, `finger://graph.no/oslo` | Gopher (and `gophers://`), Gemini and Finger in one browser. Searches, Gemini input, redirects, back and forward, bookmarks, downloads through Tasks, and trust-on-first-use certificate pins that warn when a certificate changes. |
| Inquire | `inquire:umn.edu`, `inquire:AS13335`, `inquire:@user@host`, `inquire:minuet` | Works out what you typed and asks every source that applies at once. Results stream in as they arrive. Anything typed into the command palette can be sent to Inquire. |
| NetTools | `net:ping/example.org`, `net:dns/MX/umn.edu`, `net:tls/example.org` | A small command line: `ping`, `trace`, `dns`, `whois`, `rdap`, `asn`, `ports`, `tls` and `mdns`. |
| Terminal | `term:`, `ssh:`, `ssh://me@host`, `telnet://telehack.com` | A VT220/xterm terminal for local shells, SSH, Telnet and Mosh. SSH reads `~/.ssh/config`, uses your agent and keys, checks `known_hosts`, hops through `ProxyJump` hosts and forwards ports, with post-quantum key exchange first. Ctrl+] then a key gives that key to Alouette. |
| Files | `files:`, `sftp://me@host/`, `ftp://ftp.gnu.org/gnu/`, `davs://host/dav/`, `s3://bucket/` | Two panes, each on a local folder or a server. Copy whole folders either way in the background, make folders, rename and delete. A folder's README is shown as you arrive. FTPS (explicit and implicit), WebDAV (Nextcloud and friends) and S3 (AWS, MinIO, R2) with SigV4. |
| Mail | `mail:`, `mail:setup`, `mailto:someone@example.org` | Folders, a message list and a reader in one window, and a separate window for writing. Reply, reply all, forward, star, archive, move, delete to Trash and filter. New mail arrives through IMAP IDLE. IMAP and SMTP with passwords, JMAP (Fastmail, Stalwart), and Gmail and Microsoft 365 signed in with OAuth using your own app's client ID. Passwords and tokens go to the system keychain. An offline copy is off unless you turn on `mail.offline_cache`. |
| News | `news:`, `nntps://news.example.org/comp.lang.rust` | Usenet the way Minuet read it, with threads. Lists every group on the server to pick from, keeps track of what you have read, pages with Space through unread articles, posts and follows up, and replies by mail. NNTPS or STARTTLS, and AUTHINFO sign-in with the password in your keychain. |
| Feeds | `feeds:`, `feed:https://blog.rust-lang.org/feed.xml` | RSS, Atom and JSON Feed, with podcast enclosures. Type a site's address and Alouette finds its feed. Refreshes in the background and remembers what you have read. |
| Social | `social:`, `social:@you@mastodon.social` | Mastodon and servers that speak its API (GoToSocial, Pleroma, Akkoma). Home, notifications, local and federated timelines; threads, posting with content warnings and visibility, favourites and boosts. Signs in with a pasted token or through your browser. |

## Three layouts, one set of windows

| Layout | What it looks like |
| --- | --- |
| **Desktop** | Overlapping windows with a menu bar, shadows and a patterned background, like Minuet and Turbo Vision. Drag titles to move, drag the corner to resize, double-click to zoom. |
| **Tiled** | A master window and a stack, like a tiling window manager. |
| **Tabs** | One window at a time with a tab bar. |

Switch at any time with Ctrl-F6, the Window menu or the palette. No window loses its state.

## Keys

The default keymap (`cua`) follows Minuet and Turbo Vision. `vim` and `emacs` presets are built
in, and any binding can be changed in the `[keys]` section of the config.

| Key | Does |
| --- | --- |
| Ctrl-K | Command palette: commands, windows, themes, history and URLs |
| F10 or Alt+letter | Menu bar |
| F3 | Open a URL |
| F6 / Shift-F6 | Next / previous window |
| Ctrl-F6 | Next layout |
| F5 | Zoom |
| Ctrl-F5 | Move and resize with the arrow keys |
| Ctrl-W | Close the window |
| Ctrl-T | Tasks |
| F12 | Next theme |
| F1 | Help |
| Ctrl-Q | Exit |

`alouette keys vim` prints every binding of a preset.

## Configuration

The config file is TOML and reloads as soon as you save it. `alouette config path` shows where it
lives, `alouette config default` prints every option with comments, and `alouette config check`
validates it. Set `ALOUETTE_HOME` to keep everything (config, data, cache, logs) in one folder.

Themes: `minuet`, `turbo`, `amber`, `green`, `modern-dark`, `modern-light`, `high-contrast` and
`mono`. Drop a `*.toml` file into the themes folder to add your own; it can inherit from a
built-in one and override any color.

Every nostalgia touch (splash screen, desktop pattern, shadows, sounds, Minuet module names,
tips) can be switched off one by one, or all at once with `nostalgia.enabled = false`.

## Network

Every module connects through one layer, configured in `[network]`:

- IPv6 and IPv4 raced with Happy Eyeballs (RFC 8305).
- TLS 1.3 offering the hybrid post-quantum X25519MLKEM768 key exchange first. TLS 1.2 stays
  available for older servers unless `tls_min = "1.3"`. Certificates are checked against the
  operating system's trust store; Gemini-style trust on first use is available to modules.
- Encrypted DNS: `dns = "doh"`, `"dot"` or `"doq"` with Cloudflare, Quad9, Google or your own server.
- SOCKS5 (with remote DNS for Tor) and HTTP CONNECT proxies, globally or per host. `.onion`
  names never leak to DNS: without a Tor route they are refused.

`alouette probe example.org:443` shows how a connection was made, for example:

```text
via      direct
peer     [2001:db8::443]:443
tls      TLS 1.3 · TLS13_AES_256_GCM_SHA384 · X25519MLKEM768
pq       yes (hybrid ML-KEM)
badge    IPv6 · TLS 1.3 · PQ
```

## Plan

| Milestone | Brings |
| --- | --- |
| **M0** | Foundation: shell, layouts, palette, themes, keymaps, tasks, config, storage, network layer |
| M1 | Burrow (Gopher, Gemini, Finger), Inquire (search and lookups), NetTools |
| M2 | Terminal (SSH, Telnet, Mosh) and Files (SFTP, FTP, WebDAV, S3) |
| M3 | Mail (IMAP, JMAP, Gmail, Microsoft 365, SMTP, PGP) |
| M4 | News (NNTP, RSS, Atom) and Social (Mastodon, Bluesky) |
| M5 | Web reader |
| M6 | Contacts (CardDAV, LDAP) and Calendar (CalDAV) |
| M7 | Chat (IRC, Matrix, XMPP) |
| M8 | Assistant (opt-in AI: local CLIs, local network or cloud) |
| M9 | `alouette serve`: Alouette over SSH |
| M10 | Polish, accessibility, packaging, 1.0 |

## Building

Rust 1.89 or newer. `cargo test --workspace` runs every test, including screen snapshots
(`cargo insta review` updates them after an intended change).

| Crate | Role |
| --- | --- |
| `alouette` | The binary and command line |
| `alouette-core` | Events, background tasks, URL routing, config, paths |
| `alouette-store` | SQLite state (history, bookmarks) and the OS keychain |
| `alouette-net` | Happy Eyeballs, TLS 1.3 with post-quantum key exchange, DoH/DoT/DoQ, SOCKS5 and HTTP proxies, routing rules |
| `alouette-ui` | Window manager, layouts, menus, palette, themes, keymaps, built-in pages |
| `alouette-nostalgia` | Splash art, tips and Minuet history |
| `alouette-lookup` | Lookups shared by Inquire and NetTools |
| `proto-gopher`, `proto-gemini`, `proto-finger`, `proto-whois`, `proto-dict` | Protocol clients that work over any async stream |
| `mod-burrow`, `mod-inquire`, `mod-nettools` | The modules |
| `mod-demo` | A small example module |

## License

MIT. See [LICENSE](LICENSE).
