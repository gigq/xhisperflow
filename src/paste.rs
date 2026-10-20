use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub(crate) fn wayland(text: &str, keys: &str) -> Result<()> {
    paste_with(Path::new("wl-copy"), Path::new("wtype"), text, keys)
}

fn paste_with(copy: &Path, keyboard: &Path, text: &str, keys: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let key_args: &[&str] = match keys {
        "ctrl+v" => &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
        "ctrl+shift+v" => &[
            "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
        ],
        "shift+insert" => &["-M", "shift", "-k", "Insert", "-m", "shift"],
        _ => bail!("paste-keys must be ctrl+v, ctrl+shift+v, or shift+insert"),
    };
    // wl-copy's normal background mode exits only once it owns the selection.
    // --paste-once is unsuitable: clipboard managers can consume that first read.
    let mut child = Command::new(copy)
        .args(["--type", "text/plain;charset=utf-8"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("Fast paste requires wl-copy (wl-clipboard)")?;
    let write = child
        .stdin
        .take()
        .context("Clipboard stdin unavailable")?
        .write_all(text.as_bytes());
    let status = child.wait().context("Could not wait for wl-copy")?;
    write.context("Could not write the transcript to the clipboard")?;
    if !status.success() {
        bail!("wl-copy failed ({status}); no paste shortcut was sent");
    }
    // A short device-initialization pause, then one paste, regardless of text size.
    // Leave the transcript on the clipboard;
    // early restoration can race an application's asynchronous clipboard read.
    let status = Command::new(keyboard)
        .args(["-s", "100"])
        .args(key_args)
        .stdin(Stdio::null())
        .status()
        .context("Fast paste requires wtype; the transcript remains on the clipboard")?;
    if !status.success() {
        bail!("Paste shortcut failed ({status}); the transcript remains on the clipboard");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn pastes_full_unicode_transcript_once_after_clipboard_is_ready() {
        let _guard = crate::PROCESS_FIXTURE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("copied");
        let args = dir.path().join("keys");
        let copy = script(
            dir.path(),
            "copy",
            &format!("/bin/cat > '{}'", content.display()),
        );
        let keyboard = script(
            dir.path(),
            "keyboard",
            &format!(
                "test -s '{}' || exit 9\nprintf '%s\\n' \"$@\" > '{}'",
                content.display(),
                args.display()
            ),
        );
        let text = "-- Leading dash. Café 日本語 🙂 'quotes'\nSecond paragraph.\n".repeat(100);
        paste_with(&copy, &keyboard, &text, "ctrl+v").unwrap();
        assert_eq!(fs::read_to_string(content).unwrap(), text);
        assert_eq!(
            fs::read_to_string(args).unwrap(),
            "-s\n100\n-M\nctrl\n-k\nv\n-m\nctrl\n"
        );
    }

    #[test]
    fn failed_copy_never_sends_a_paste() {
        let _guard = crate::PROCESS_FIXTURE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let copy = script(dir.path(), "copy", "/bin/cat > /dev/null\nexit 8");
        let marker = dir.path().join("pasted");
        let keyboard = script(
            dir.path(),
            "keyboard",
            &format!("touch '{}'", marker.display()),
        );
        assert!(paste_with(&copy, &keyboard, "test", "ctrl+v").is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn invalid_shortcut_cannot_change_clipboard() {
        assert!(
            paste_with(
                Path::new("/not/a/tool"),
                Path::new("/not/a/tool"),
                "test",
                "invalid"
            )
            .unwrap_err()
            .to_string()
            .contains("paste-keys")
        );
    }
}
