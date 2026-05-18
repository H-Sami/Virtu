use crate::vm::profile::VmView;
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();

    writeln!(
        xml,
        "    <controller type='usb' model='qemu-xhci' ports='15'/>"
    )?;

    if let Some(path) = view.input.keyboard_evdev {
        writeln!(xml, "    <input type='evdev'>")?;
        writeln!(
            xml,
            "      <source dev='{}' grab='all' grabToggle='ctrl-ctrl' repeat='on'/>",
            path.display()
        )?;
        writeln!(xml, "    </input>")?;
    }

    if let Some(path) = view.input.mouse_evdev {
        writeln!(xml, "    <input type='evdev'>")?;
        writeln!(xml, "      <source dev='{}'/>", path.display())?;
        writeln!(xml, "    </input>")?;
    }

    for path in &view.input.additional_evdev {
        writeln!(xml, "    <input type='evdev'>")?;
        writeln!(xml, "      <source dev='{}'/>", path.display())?;
        writeln!(xml, "    </input>")?;
    }

    if view.input.keyboard_evdev.is_none() {
        writeln!(xml, "    <input type='tablet' bus='usb'/>")?;
    }

    Ok(xml)
}
