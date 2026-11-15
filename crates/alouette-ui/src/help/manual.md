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

Alouette opens these URLs today: about:welcome, about:tasks, about:history, about:settings, about:keys, about:help, about:alouette, about:minuet, and demo: pages from the demo module. Each later milestone adds schemes: gopher, gemini and finger in M1, ssh and sftp in M2, mailto and imap in M3, and so on.

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
