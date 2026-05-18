use crate::vm::profile::{NetworkType, VmProfile};
use crate::vm::xml::XmlError;
use std::fmt::Write as FmtWrite;

pub fn render(profile: &VmProfile) -> Result<String, XmlError> {
    let mut xml = String::new();
    let queues = profile.vcpu_count.min(8);

    match &profile.network_type {
        NetworkType::Nat => {
            writeln!(xml, "    <interface type='network'>")?;
            writeln!(xml, "      <source network='default'/>")?;
            writeln!(xml, "      <model type='virtio'/>")?;
            writeln!(xml, "      <driver name='vhost' queues='{queues}'/>")?;
            writeln!(xml, "    </interface>")?;
        }
        NetworkType::Bridge { interface } => {
            writeln!(xml, "    <interface type='bridge'>")?;
            writeln!(xml, "      <source bridge='{interface}'/>")?;
            writeln!(xml, "      <model type='virtio'/>")?;
            writeln!(xml, "      <driver name='vhost' queues='{queues}'/>")?;
            writeln!(xml, "    </interface>")?;
        }
    }

    Ok(xml)
}
