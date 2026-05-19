use crate::vm::xml::XmlError;
use crate::vm::{NetworkChoice, VmView};
use std::fmt::Write as FmtWrite;

pub fn render(view: &VmView<'_>) -> Result<String, XmlError> {
    let mut xml = String::new();
    let queues = view.vcpu_count.min(8);

    match view.network {
        NetworkChoice::Nat => {
            writeln!(xml, "    <interface type='network'>")?;
            writeln!(xml, "      <source network='default'/>")?;
            writeln!(xml, "      <model type='virtio'/>")?;
            writeln!(xml, "      <driver name='vhost' queues='{queues}'/>")?;
            writeln!(xml, "    </interface>")?;
        }
        NetworkChoice::Bridge { interface } => {
            writeln!(xml, "    <interface type='bridge'>")?;
            writeln!(xml, "      <source bridge='{interface}'/>")?;
            writeln!(xml, "      <model type='virtio'/>")?;
            writeln!(xml, "      <driver name='vhost' queues='{queues}'/>")?;
            writeln!(xml, "    </interface>")?;
        }
        NetworkChoice::None => {}
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
    use crate::vm::NetworkChoice;

    /// NAT default emits the libvirt `default` network with virtio +
    /// vhost driver and a queue count clamped to vcpu_count (4 here).
    #[test]
    fn network_renderer_nat_emits_exact_block() {
        let profile = amd_host_with_amd_passthrough();
        let config = windows_dual_gpu_config_amd_passthrough();
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        let expected = "    <interface type='network'>\n      \
                        <source network='default'/>\n      \
                        <model type='virtio'/>\n      \
                        <driver name='vhost' queues='4'/>\n    \
                        </interface>\n";
        assert_eq!(xml, expected);
    }

    /// Bridge mode emits the bridge interface name verbatim (no
    /// validation here — that lives in vm/validation.rs).
    #[test]
    fn network_renderer_bridge_emits_named_bridge() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.network = NetworkChoice::Bridge {
            interface: "br0".to_string(),
        };
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.contains("<interface type='bridge'>"));
        assert!(xml.contains("<source bridge='br0'/>"));
    }

    /// `NetworkChoice::None` produces an empty fragment so the libvirt
    /// domain has no network interface at all.
    #[test]
    fn network_renderer_none_emits_empty_fragment() {
        let profile = amd_host_with_amd_passthrough();
        let mut config = windows_dual_gpu_config_amd_passthrough();
        config.network = NetworkChoice::None;
        let view = vm_view(&profile, &config).expect("view");

        let xml = render(&view).expect("render");
        assert!(xml.is_empty());
    }
}
