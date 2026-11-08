// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! Key chords: `Hyper+R`, `cmd+k`, `return`, read into one canonical form
//! and spelled the way each backend wants it.

/// Modifiers in canonical order.
const MODIFIERS: [&str; 4] = ["ctrl", "option", "shift", "cmd"];

/// A chord as canonical names: modifiers in `ctrl, option, shift, cmd`
/// order, then exactly one key (`r`, `return`, `escape`, `f5`, ...).
/// `None` when the text is not a chord: an unknown name, no key, two keys.
///
/// `Hyper` is all four modifiers. A bare key counts only for the named
/// keys a goal would spell out (`return`, `escape`, ...); a lone letter is
/// a word, not a chord.
pub fn parse(text: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = text
        .trim()
        .split('+')
        .map(|part| part.trim().to_lowercase())
        .collect();
    let (key, modifiers) = parts.split_last()?;
    let mut held = [false; 4];
    for modifier in modifiers {
        match modifier.as_str() {
            "hyper" => held = [true; 4],
            "ctrl" | "control" | "⌃" => held[0] = true,
            "option" | "opt" | "alt" | "⌥" => held[1] = true,
            "shift" | "⇧" => held[2] = true,
            "cmd" | "command" | "meta" | "⌘" => held[3] = true,
            _ => return None,
        }
    }
    let key = match key.as_str() {
        "enter" | "return" => "return".to_string(),
        "esc" | "escape" => "escape".to_string(),
        "backspace" | "delete" => "delete".to_string(),
        "arrowup" | "up" => "up".to_string(),
        "arrowdown" | "down" => "down".to_string(),
        "arrowleft" | "left" => "left".to_string(),
        "arrowright" | "right" => "right".to_string(),
        "tab" | "space" | "home" | "end" | "pageup" | "pagedown" => key.clone(),
        f if f.len() <= 3
            && f.starts_with('f')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            f.to_string()
        }
        single if single.chars().count() == 1 => single.to_string(),
        _ => return None,
    };
    let bare_named = !held.contains(&true);
    if bare_named
        && !matches!(
            key.as_str(),
            "return" | "escape" | "tab" | "space" | "delete"
        )
    {
        return None;
    }
    let mut out: Vec<String> = MODIFIERS
        .iter()
        .zip(held)
        .filter(|(_, on)| *on)
        .map(|(name, _)| (*name).to_string())
        .collect();
    out.push(key);
    Some(out)
}

/// The chord as agent-browser (Playwright) spells it: `Control+Alt+Shift+Meta+r`,
/// `Enter`. Text that is not a chord is passed through unchanged, so a key
/// agent-browser already understands (`ArrowDown`, `PageDown`) keeps working.
pub fn for_browser(text: &str) -> String {
    let Some(keys) = parse(text) else {
        return text.to_string();
    };
    keys.iter()
        .map(|key| match key.as_str() {
            "ctrl" => "Control".to_string(),
            "option" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "cmd" => "Meta".to_string(),
            "return" => "Enter".to_string(),
            "escape" => "Escape".to_string(),
            "delete" => "Backspace".to_string(),
            "tab" => "Tab".to_string(),
            "space" => "Space".to_string(),
            "up" => "ArrowUp".to_string(),
            "down" => "ArrowDown".to_string(),
            "left" => "ArrowLeft".to_string(),
            "right" => "ArrowRight".to_string(),
            "home" => "Home".to_string(),
            "end" => "End".to_string(),
            "pageup" => "PageUp".to_string(),
            "pagedown" => "PageDown".to_string(),
            f if f.starts_with('f') && f.len() > 1 => f.to_uppercase(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Option<String> {
        parse(text).map(|keys| keys.join(" "))
    }

    #[test]
    fn chords_read_into_one_canonical_form() {
        assert_eq!(keys("Hyper+R"), Some("ctrl option shift cmd r".into()));
        assert_eq!(keys("cmd+K"), Some("cmd k".into()));
        assert_eq!(keys("Shift+Control+tab"), Some("ctrl shift tab".into()));
        assert_eq!(keys("Return"), Some("return".into()));
        assert_eq!(keys("Escape"), Some("escape".into()));
        assert_eq!(
            keys("ArrowDown"),
            None,
            "a bare arrow is not a chord a goal names"
        );
        assert_eq!(keys("cmd+F5"), Some("cmd f5".into()));
        // Words, not chords.
        for word in ["R", "Raycast", "Type", "cmd+", "+r", "super+r", "cmd+kk"] {
            assert_eq!(keys(word), None, "{word}");
        }
    }

    #[test]
    fn the_browser_spelling_is_playwrights() {
        assert_eq!(for_browser("Hyper+R"), "Control+Alt+Shift+Meta+r");
        assert_eq!(for_browser("cmd+k"), "Meta+k");
        assert_eq!(for_browser("return"), "Enter");
        assert_eq!(for_browser("Enter"), "Enter");
        // Not a chord: untouched.
        assert_eq!(for_browser("ArrowDown"), "ArrowDown");
        assert_eq!(for_browser("PageDown"), "PageDown");
    }
}
