use crate::vm::profile::VmProfile;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(profile: &VmProfile) -> Result<String, XmlError> {
    let mut xml = String::new();

    writeln!(
        xml,
        "    <controller type='usb' model='qemu-xhci' ports='15'/>"
    )?;

    if let Some(kbd) = &profile.evdev_keyboard {
        if let Some(path) = &kbd.evdev_path {
            writeln!(xml, "    <input type='evdev'>")?;
            writeln!(
                xml,
                "      <source dev='{}' grab='all' grabToggle='ctrl-ctrl' repeat='on'/>",
                path.display()
            )?;
            writeln!(xml, "    </input>")?;
        }
    }

    if let Some(mouse) = &profile.evdev_mouse {
        if let Some(path) = &mouse.evdev_path {
            writeln!(xml, "    <input type='evdev'>")?;
            writeln!(xml, "      <source dev='{}'/>", path.display())?;
            writeln!(xml, "    </input>")?;
        }
    }

    if profile.evdev_keyboard.is_none() {
        writeln!(xml, "    <input type='tablet' bus='usb'/>")?;
    }

    Ok(xml)
}
