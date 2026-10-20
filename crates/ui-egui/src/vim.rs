//! Vim keys in the document (File › Options › Advanced, `vim.toggle`): in Normal and Visual
//! mode the keys go to `vim.keys`; Insert mode types as usual until Escape. Shortcuts with ⌘ or
//! Ctrl (bold, save, find…) and Home, End, Page Up, Page Down and the function keys keep their
//! usual meaning in every mode.

use egui::{Event, Key, Modifiers};
use serde_json::json;
use wordcraft_engine::cmd::vim::Mode;

use crate::WordApp;

/// Handle `e` as a Vim key. True when it was one (nothing else should act on it).
pub(crate) fn event(app: &mut WordApp, ctx: &egui::Context, e: &Event) -> bool {
    if !app.session.prefs.vim_keys || app.session.math.is_some() {
        return false;
    }
    let insert = app.session.vim.mode == Mode::Insert;
    match e {
        // Escape still puts the format painter down and leaves a composition first.
        Event::Key { key: Key::Escape, pressed: true, modifiers, .. } if modifiers.is_none() => {
            app.session.painter.is_none() && app.canvas.ime_preedit.is_empty() && send(app, "<Esc>")
        }
        _ if insert => false,
        Event::Text(t) => {
            let m = ctx.input(|i| i.modifiers);
            if m.command || (m.ctrl && !cfg!(target_os = "macos")) {
                return false;
            }
            send_text(app, t);
            true
        }
        Event::Ime(egui::ImeEvent::Commit(t)) => {
            app.canvas.ime_preedit.clear();
            send_text(app, t);
            true
        }
        Event::Ime(egui::ImeEvent::Preedit { .. }) => true,
        Event::Key { key, pressed: true, modifiers, .. } => key_event(app, *key, *modifiers),
        _ => false,
    }
}

/// A key press in Normal or Visual mode. Keys that type arrive again as text (handled there),
/// so here they're only kept from running a shortcut or editing.
fn key_event(app: &mut WordApp, key: Key, m: Modifiers) -> bool {
    let ctrl_only = m.ctrl && !m.alt && !m.shift && !(cfg!(target_os = "macos") && m.mac_cmd);
    if ctrl_only && app.session.vim.mode == Mode::Normal {
        // Ctrl+R redo, Ctrl+D / Ctrl+U half a page, Ctrl+F / Ctrl+B a page.
        let c = match key {
            Key::R => 'r',
            Key::D => 'd',
            Key::U => 'u',
            Key::F => 'f',
            Key::B => 'b',
            _ => return false,
        };
        send(app, &format!("<C-{c}>"));
        return true;
    }
    if m.command || m.ctrl || m.alt {
        return false;
    }
    let object = crate::objects::selected(app).is_some();
    let named = match key {
        Key::ArrowLeft if !m.shift && !object => "<Left>",
        Key::ArrowRight if !m.shift && !object => "<Right>",
        Key::ArrowUp if !m.shift && !object => "<Up>",
        Key::ArrowDown if !m.shift && !object => "<Down>",
        Key::Enter if !m.shift => "<CR>",
        Key::Backspace => "<BS>",
        Key::Delete if !object => "<Del>",
        // Shift+arrows select as usual (Visual mode takes the selection); the others move or
        // run their shortcuts.
        Key::ArrowLeft
        | Key::ArrowRight
        | Key::ArrowUp
        | Key::ArrowDown
        | Key::Delete
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::Insert => {
            return false;
        }
        k if crate::keys::key_name(k).is_some_and(|n| n.starts_with('F') && n.len() > 1) => return false,
        // Letters, digits, Space, Tab, Shift+Enter: nothing but the text they send.
        _ => return true,
    };
    send(app, named);
    true
}

fn send_text(app: &mut WordApp, t: &str) {
    let keys: String =
        t.chars().filter(|c| !c.is_control() || *c == '\t').map(|c| if c == '<' { "<lt>".to_string() } else { c.to_string() }).collect();
    if !keys.is_empty() {
        send(app, &keys);
    }
}

/// Run `vim.keys`; whether the keys meant something.
fn send(app: &mut WordApp, keys: &str) -> bool {
    match app.run("vim.keys", json!({"keys": keys})) {
        Ok(r) => r.get("handled").and_then(|h| h.as_bool()).unwrap_or(true),
        Err(_) => false,
    }
}

/// The status bar's mode, with any unfinished command after it (`NORMAL  2d`).
pub(crate) fn status(app: &WordApp) -> Option<String> {
    if !app.session.prefs.vim_keys {
        return None;
    }
    let mode = match app.session.vim.mode {
        Mode::Normal => tl!("NORMAL"),
        Mode::Insert => tl!("INSERT"),
        Mode::Visual => tl!("VISUAL"),
        Mode::VisualLine => tl!("VISUAL LINE"),
    };
    let pending = app.session.vim.pending();
    Some(if pending.is_empty() { format!("-- {mode} --") } else { format!("-- {mode} --  {pending}") })
}
