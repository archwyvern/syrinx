//! The `.syr` file association on Windows, per user, no elevation: `syrinx-player --register`
//! and `--unregister`. Linux gets its association from `make install-user` (a desktop entry and
//! the shared MIME type), so there this is a pointer to that.

use std::path::Path;

use anyhow::Result;

#[cfg(windows)]
pub fn register(exe: &Path) -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let exe = exe.display().to_string();
    let classes = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Software\\Classes", winreg::enums::KEY_ALL_ACCESS)?;
    let (ext, _) = classes.create_subkey(".syr")?;
    ext.set_value("", &"syrinx.source")?;
    ext.set_value("Content Type", &"audio/x-syrinx")?;
    ext.set_value("PerceivedType", &"audio")?;
    let (prog, _) = classes.create_subkey("syrinx.source")?;
    prog.set_value("", &"syrinx sound source")?;
    let (icon, _) = prog.create_subkey("DefaultIcon")?;
    icon.set_value("", &format!("{exe},0"))?;
    let (command, _) = prog.create_subkey("shell\\open\\command")?;
    command.set_value("", &format!("\"{exe}\" \"%1\""))?;
    println!("registered .syr -> {exe}");
    println!("Explorer picks the change up on its next start; sign out and in if it does not.");
    Ok(())
}

#[cfg(windows)]
pub fn unregister() -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let classes = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Software\\Classes", winreg::enums::KEY_ALL_ACCESS)?;
    let _ = classes.delete_subkey_all("syrinx.source");
    let _ = classes.delete_subkey_all(".syr");
    println!("unregistered .syr");
    Ok(())
}

#[cfg(not(windows))]
pub fn register(_exe: &Path) -> Result<()> {
    anyhow::bail!("on Linux the .syr association is installed by `make install-user` in the syrinx repository")
}

#[cfg(not(windows))]
pub fn unregister() -> Result<()> {
    anyhow::bail!("on Linux the .syr association is installed by `make install-user` in the syrinx repository")
}
