//! The kernel spec Jupyter starts the kernel by: `kernels/<name>/` with
//! `kernel.json` and the logos.

use nabic_diagnostic::Json;
use std::path::{Path, PathBuf};

/// Write kernel `name` (`display` in menus) running `argv`, with
/// `NABI_HOME` set to `nabi_home`, under `$JUPYTER_DATA_DIR` (else the
/// per-user default); its `kernel.json`.
pub fn install(
    name: &str,
    display: String,
    argv: Vec<String>,
    nabi_home: &Path,
) -> Result<PathBuf, String> {
    let user_home = nabic_core::user_home;
    let data = std::env::var_os("JUPYTER_DATA_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(target_vendor = "apple") {
                Some(user_home()?.join("Library/Jupyter"))
            } else if cfg!(windows) {
                std::env::var_os("APPDATA")
                    .filter(|d| !d.is_empty())
                    .map(|d| PathBuf::from(d).join("jupyter"))
            } else {
                std::env::var_os("XDG_DATA_HOME")
                    .filter(|d| !d.is_empty())
                    .map(|d| PathBuf::from(d).join("jupyter"))
                    .or_else(|| Some(user_home()?.join(".local/share/jupyter")))
            }
        })
        .ok_or("cannot find the Jupyter data directory (set JUPYTER_DATA_DIR)")?;
    let dir = data.join("kernels").join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut spec = vec![
        (
            "argv".to_owned(),
            Json::Arr(argv.into_iter().map(Json::Str).collect()),
        ),
        ("display_name".to_owned(), Json::Str(display)),
        ("language".to_owned(), Json::str("nabi")),
        ("interrupt_mode".to_owned(), Json::str("message")),
    ];
    // A GUI-started Jupyter has a bare PATH; keep this shell's.
    let mut env = vec![(
        "NABI_HOME".to_owned(),
        Json::Str(nabi_home.display().to_string()),
    )];
    if let Ok(path) = std::env::var("PATH") {
        env.push(("PATH".to_owned(), Json::Str(path)));
    }
    spec.push(("env".to_owned(), Json::Obj(env)));
    let file = dir.join("kernel.json");
    std::fs::write(&file, format!("{}\n", Json::Obj(spec)))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    // The kernel's icons, from the repository's assets.
    for (logo, png) in [
        (
            "logo-32x32.png",
            &include_bytes!("../../../assets/icons/jupyter/logo-32x32.png")[..],
        ),
        (
            "logo-64x64.png",
            &include_bytes!("../../../assets/icons/jupyter/logo-64x64.png")[..],
        ),
        (
            "logo-svg.svg",
            &include_bytes!("../../../assets/icons/jupyter/logo-svg.svg")[..],
        ),
    ] {
        let logo = dir.join(logo);
        std::fs::write(&logo, png).map_err(|e| format!("{}: {e}", logo.display()))?;
    }
    Ok(file)
}
