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

#[cfg(test)]
mod tests {
    use super::render;
    use crate::vm::profile::vm_view;
    use crate::vm::xml::devices::fixtures::{
        amd_host_with_amd_passthrough, windows_dual_gpu_config_amd_passthrough,
    };
    use std::path::PathBuf;

    /// With no evdev passthrough configured, the renderer falls back to
    /// a USB tablet so the user has *some* pointer when virt-viewer or
    /// SPICE is connected. The xhci controller always ships.
    #[test]
    fn input_renderer_emits_tablet_fallback_when_no_evdev() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        let expected = "    <controller type='usb' model='qemu-xhci' ports='15'/>\n    \
                        <input type='tablet' bus='usb'/>\n";
        assert_eq!(xml, expected);
    }

    /// Configured keyboard evdev: emit the `<input type='evdev'>` block
    /// with the documented `grab='all' grabToggle='ctrl-ctrl'
    /// repeat='on'` triple. These flags are critical for VFIO usability
    /// because they let the user release the keyboard back to the host
    /// with both Ctrls and pass autorepeat through to the guest.
    #[test]
    fn input_renderer_emits_keyboard_evdev_with_grab_toggle() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.input.keyboard_evdev =
            Some(PathBuf::from("/dev/input/by-id/usb-keyboard-event-kbd"));
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<input type='evdev'>"));
        assert!(xml.contains(
            "<source dev='/dev/input/by-id/usb-keyboard-event-kbd' \
             grab='all' grabToggle='ctrl-ctrl' repeat='on'/>"
        ));
        // Once a keyboard is attached, the tablet fallback drops away.
        assert!(!xml.contains("<input type='tablet'"));
    }

    /// Mouse and any additional evdev devices (e.g. wheel, gamepad)
    /// each get their own minimal `<input type='evdev'>` block. We pin
    /// that order is preserved so the user-supplied list maps directly
    /// to the XML.
    #[test]
    fn input_renderer_emits_mouse_and_additional_evdev_in_order() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.input.mouse_evdev = Some(PathBuf::from("/dev/input/by-id/usb-mouse-event-mouse"));
        config.input.additional_evdev = vec![
            PathBuf::from("/dev/input/by-id/usb-pad-event-joystick"),
            PathBuf::from("/dev/input/by-id/usb-wheel-event-joystick"),
        ];
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        let mouse = xml
            .find("usb-mouse-event-mouse")
            .expect("mouse evdev present");
        let pad = xml
            .find("usb-pad-event-joystick")
            .expect("pad evdev present");
        let wheel = xml
            .find("usb-wheel-event-joystick")
            .expect("wheel evdev present");
        assert!(mouse < pad);
        assert!(pad < wheel);
    }
}
