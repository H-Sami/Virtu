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
