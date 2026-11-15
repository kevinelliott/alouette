# Alouette manual

## The idea

Alouette puts your whole internet desk in one terminal: mail, news, Gopher and Gemini, file transfer, terminals, the web, search and more. It follows Minuet's three big ideas: one reader for everything you read, everything runs in the background, and every URL opens in the right built-in tool.

## Layouts

Desktop is the Minuet look: a menu bar, overlapping windows you can drag, zoom and resize, and drop shadows. Tiled fills the screen with a main pane and a stack. Tabs shows one window at a time with a tab bar, which suits small terminals and screen readers. Switch at any time from the Window menu, the palette, or the layout key. Nothing is lost when you switch.

## The command palette

Press Ctrl+K (or the palette key for your keymap) and start typing. The palette finds commands, open windows, modules, themes, keymaps and recent URLs. If what you type looks like a URL, the first entry opens it.

## Windows

Every window has a number. Alt plus that number focuses it. In the Desktop layout, drag a title bar to move a window, drag its bottom-right corner to resize it, click [■] to close it and [↑] to zoom it. From the keyboard, the move key enters move mode: arrow keys move, Shift plus arrow keys resize, and Enter or Esc finishes.

## Background tasks

Network work runs as tasks. The Tasks window lists them with progress. Select one and press c to cancel, r to retry, or x to clear finished tasks.

## URLs

Alouette opens these URLs today: about: pages (welcome, tasks, history, settings, keys, help), gopher, gophers, gemini and finger (Burrow), inquire (Inquire), net (NetTools), term, ssh, telnet and mosh (Terminal), and files, file, sftp, ftp, ftps, ftpes, dav, davs and s3 (Files). Each later milestone adds more: mailto and imap in M3, news in M4, and so on.

## Terminal

term: opens a shell on this computer. ssh: lists the hosts in your ~/.ssh/config and the ones saved under [[ssh.hosts]]; ssh://user@host connects. Alouette uses your SSH agent, your key files (asking for passphrases), known_hosts, ProxyJump and LocalForward, and asks before trusting a new host key. A changed host key is refused.

While a terminal has the keyboard, every key goes to the program inside it. Press Ctrl+] and then a key to give that key to Alouette instead: Ctrl+] then k opens the palette, Ctrl+] then F6 goes to the next window. Shift+PgUp and Shift+PgDn scroll back. When a session ends, Enter closes the window and r reconnects.

telnet://host connects with Telnet, VT100 and all. mosh://user@host runs the mosh program if it is installed.

## Files

Files shows two panes. The left starts in your home folder; the right shows the URL you opened, such as sftp://me@host/, ftp://ftp.gnu.org/gnu/, davs://cloud.example.com/remote.php/dav/files/me/ or s3://bucket/. Save sites under [[files.sites]] and open them with files:name.

Tab switches panes. Enter opens a folder or shows a file, Backspace goes up, Space marks. c (or F5) copies the marked items, or the selected one, to the other pane, in the background; watch them in Tasks. n makes a folder, r renames, d deletes (after asking), o opens a path or URL in this pane, h shows hidden files, g reloads. A README in a folder is shown as you arrive, as Minuet did.

FTP passwords travel in the clear: use ftpes:// (TLS on port 21) or ftps:// (TLS on port 990) when the server offers it. S3 keys come from AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY or ~/.aws/credentials; set an endpoint and region on a saved site for MinIO, R2 and other stores.

## Configuration

Settings live in alouette.toml. Run "alouette config path" to find it. Alouette reloads the file as soon as you save it. The Settings window changes common options for you and saves the file.

## Themes

Built-in themes: minuet, turbo, amber, green, modern-dark, modern-light, high-contrast and mono. Put your own theme files in the themes folder next to alouette.toml. A theme can inherit a built-in and override any color slot.

## Keymaps

Three presets ship: cua (F-keys and Alt menus, like Minuet), vim (Ctrl+W window commands and : for the palette) and emacs (C-x prefixes and M-x for the palette). Override any binding in the [keys] table.

## Nostalgia

The splash screen, drop shadows, desktop pattern, Minuet's module names, sounds and retro commands are each a switch in [nostalgia]. Set enabled = false to turn them all off.

## Privacy

Alouette sends no telemetry. The offline cache and AI features are off until you turn them on. Passwords and tokens are kept in your operating system's keychain.
