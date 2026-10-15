use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
};

fn utf8_charmap(locale: Option<&str>) -> io::Result<bool> {
    // Query the system libc used by the editor, including when SynCodex is musl-linked.
    let mut probe = Command::new("locale");
    probe.arg("charmap").stdin(Stdio::null());
    if let Some(locale) = locale {
        probe.env("LC_ALL", locale);
    }
    let output = probe.output()?;
    let charmap = String::from_utf8_lossy(&output.stdout);
    Ok(output.status.success()
        && (charmap.trim().eq_ignore_ascii_case("UTF-8")
            || charmap.trim().eq_ignore_ascii_case("UTF8")))
}

fn configure_locale(editor: &mut Command) -> Result<()> {
    let fallback = if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    };
    match utf8_charmap(None) {
        Ok(true) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Minimal musl images may have nano without the locale utility.
            editor.env("LC_ALL", fallback);
            return Ok(());
        }
        Err(error) => return Err(error).context("Could not check the editor's character encoding"),
        Ok(false) => {}
    }
    let mut candidates = vec![fallback.to_owned(), "C.utf8".into(), "en_US.UTF-8".into()];
    let available = Command::new("locale")
        .arg("-a")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .context("Could not list the editor's available locales")?;
    if available.status.success() {
        candidates.extend(
            String::from_utf8_lossy(&available.stdout)
                .lines()
                .map(str::trim)
                .filter(|name| name.to_ascii_lowercase().replace('-', "").contains("utf8"))
                .map(str::to_owned),
        );
    }
    for locale in candidates {
        if utf8_charmap(Some(&locale))? {
            // Keep the parent shell and every other session's environment unchanged.
            editor.env("LC_ALL", locale);
            return Ok(());
        }
    }
    bail!(
        "UTF-8 ロケールを利用できません。UTF-8 ロケールを導入するか、syncodex rewind --editor builtin を使用してください。履歴は変更していません。"
    )
}

pub fn edit(text: &str, command: &str) -> Result<Option<String>> {
    let parts = shell_words::split(command).context("Invalid editor command")?;
    let program = parts.first().filter(|part| !part.is_empty());
    let Some(program) = program else {
        bail!("Empty editor command");
    };
    let mut file = tempfile::Builder::new()
        .prefix("syncodex-edit-")
        .suffix(".md")
        .tempfile()?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    let modified = fs::metadata(file.path())?.modified()?;
    let mut editor = Command::new(program);
    editor.args(&parts[1..]).arg(file.path());
    configure_locale(&mut editor)?;
    let status = editor.status().map_err(|error| {
        let message = if error.kind() == io::ErrorKind::NotFound
            && Path::new(program).file_name().is_some_and(|name| name == "nano")
        {
            "nano が見つかりません。インストーラーを再実行すると nano も導入します: curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh\nすぐに内蔵エディターを使う場合: syncodex rewind --editor builtin".to_owned()
        } else {
            format!("Could not open editor {command}; use --editor COMMAND to choose another editor")
        };
        anyhow::Error::new(error).context(message)
    })?;
    ensure!(
        status.success(),
        "Editor exited with {status}; no history changed / 履歴は変更していません。"
    );
    let edited = fs::read_to_string(file.path()).context("Cannot read the editor's UTF-8 text")?;
    let saved = fs::metadata(file.path())?.modified()? != modified || edited != text;
    Ok(saved.then_some(edited))
}
