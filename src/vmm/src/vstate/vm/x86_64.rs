use std::fmt;

use kvm_bindings::{
    kvm_clock_data, kvm_irqchip, kvm_pit_config, kvm_pit_state2, KVM_CLOCK_TSC_STABLE,
    KVM_IRQCHIP_IOAPIC, KVM_IRQCHIP_PIC_MASTER, KVM_IRQCHIP_PIC_SLAVE, KVM_PIT_SPEAKER_DUMMY,
};
use kvm_ioctls::VmFd;
use serde::{Deserialize, Serialize};

use crate::vstate::kvm::Kvm;

/// Error type for [`Vm::restore_state`]
#[allow(missing_docs)]
#[cfg(target_arch = "x86_64")]
#[derive(Debug, PartialEq, Eq, thiserror::Error, displaydoc::Display)]
pub enum ArchVmError {
    /// Set PIT2 error: {0}
    SetPit2(kvm_ioctls::Error),
    /// Set clock error: {0}
    SetClock(kvm_ioctls::Error),
    /// Set IrqChipPicMaster error: {0}
    SetIrqChipPicMaster(kvm_ioctls::Error),
    /// Set IrqChipPicSlave error: {0}
    SetIrqChipPicSlave(kvm_ioctls::Error),
    /// Set IrqChipIoAPIC error: {0}
    SetIrqChipIoAPIC(kvm_ioctls::Error),
    /// Failed to get KVM vm pit state: {0}
    VmGetPit2(kvm_ioctls::Error),
    /// Failed to get KVM vm clock: {0}
    VmGetClock(kvm_ioctls::Error),
    /// Failed to get KVM vm irqchip: {0}
    VmGetIrqChip(kvm_ioctls::Error),
    /// Failed to set KVM vm pit state: {0}
    VmSetPit2(kvm_ioctls::Error),
    /// Failed to set KVM vm clock: {0}
    VmSetClock(kvm_ioctls::Error),
    /// Failed to set KVM vm irqchip: {0}
    VmSetIrqChip(kvm_ioctls::Error),
}

/// Structure representing the current architecture's understand of what a "virtual machine" is.
#[derive(Debug)]
pub struct ArchVm {
    /// KVM handle to this VM
    pub fd: VmFd,
}

impl ArchVm {
    pub(super) fn arch_create(_: &Kvm, fd: VmFd) -> Result<ArchVm, ArchVmError> {
        Ok(Self { fd })
    }

    /// Restores the KVM VM state.
    ///
    /// # Errors
    ///
    /// When:
    /// - [`kvm_ioctls::VmFd::set_pit`] errors.
    /// - [`kvm_ioctls::VmFd::set_clock`] errors.
    /// - [`kvm_ioctls::VmFd::set_irqchip`] errors.
    /// - [`kvm_ioctls::VmFd::set_irqchip`] errors.
    /// - [`kvm_ioctls::VmFd::set_irqchip`] errors.
    pub fn restore_state(&mut self, state: &VmState) -> Result<(), ArchVmError> {
        self.fd
            .set_pit2(&state.pitstate)
            .map_err(ArchVmError::SetPit2)?;
        self.fd
            .set_clock(&state.clock)
            .map_err(ArchVmError::SetClock)?;
        self.fd
            .set_irqchip(&state.pic_master)
            .map_err(ArchVmError::SetIrqChipPicMaster)?;
        self.fd
            .set_irqchip(&state.pic_slave)
            .map_err(ArchVmError::SetIrqChipPicSlave)?;
        self.fd
            .set_irqchip(&state.ioapic)
            .map_err(ArchVmError::SetIrqChipIoAPIC)?;
        Ok(())
    }

    /// Creates the irq chip and an in-kernel device model for the PIT.
    pub fn setup_irqchip(&self) -> Result<(), ArchVmError> {
        self.fd
            .create_irq_chip()
            .map_err(ArchVmError::VmSetIrqChip)?;
        // We need to enable the emulation of a dummy speaker port stub so that writing to port 0x61
        // (i.e. KVM_SPEAKER_BASE_ADDRESS) does not trigger an exit to user space.
        let pit_config = kvm_pit_config {
            flags: KVM_PIT_SPEAKER_DUMMY,
            ..Default::default()
        };
        self.fd
            .create_pit2(pit_config)
            .map_err(ArchVmError::VmSetIrqChip)
    }

    /// Saves and returns the Kvm Vm state.
    pub fn save_state(&self) -> Result<VmState, ArchVmError> {
        let pitstate = self.fd.get_pit2().map_err(ArchVmError::VmGetPit2)?;

        let mut clock = self.fd.get_clock().map_err(ArchVmError::VmGetClock)?;
        // This bit is not accepted in SET_CLOCK, clear it.
        clock.flags &= !KVM_CLOCK_TSC_STABLE;

        let mut pic_master = kvm_irqchip {
            chip_id: KVM_IRQCHIP_PIC_MASTER,
            ..Default::default()
        };
        self.fd
            .get_irqchip(&mut pic_master)
            .map_err(ArchVmError::VmGetIrqChip)?;

        let mut pic_slave = kvm_irqchip {
            chip_id: KVM_IRQCHIP_PIC_SLAVE,
            ..Default::default()
        };
        self.fd
            .get_irqchip(&mut pic_slave)
            .map_err(ArchVmError::VmGetIrqChip)?;

        let mut ioapic = kvm_irqchip {
            chip_id: KVM_IRQCHIP_IOAPIC,
            ..Default::default()
        };
        self.fd
            .get_irqchip(&mut ioapic)
            .map_err(ArchVmError::VmGetIrqChip)?;

        Ok(VmState {
            pitstate,
            clock,
            pic_master,
            pic_slave,
            ioapic,
        })
    }
}

#[derive(Default, Deserialize, Serialize)]
/// Structure holding VM kvm state.
pub struct VmState {
    pitstate: kvm_pit_state2,
    clock: kvm_clock_data,
    // TODO: rename this field to adopt inclusive language once Linux updates it, too.
    pic_master: kvm_irqchip,
    // TODO: rename this field to adopt inclusive language once Linux updates it, too.
    pic_slave: kvm_irqchip,
    ioapic: kvm_irqchip,
}

impl fmt::Debug for VmState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmState")
            .field("pitstate", &self.pitstate)
            .field("clock", &self.clock)
            .field("pic_master", &"?")
            .field("pic_slave", &"?")
            .field("ioapic", &"?")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use kvm_bindings::{
        KVM_CLOCK_TSC_STABLE, KVM_IRQCHIP_IOAPIC, KVM_IRQCHIP_PIC_MASTER, KVM_IRQCHIP_PIC_SLAVE,
        KVM_PIT_SPEAKER_DUMMY,
    };

    use crate::snapshot::Snapshot;
    use crate::vstate::vm::tests::{setup_vm, setup_vm_with_memory};
    use crate::vstate::vm::VmState;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_vm_save_restore_state() {
        let (_, vm) = setup_vm();
        // Irqchips, clock and pitstate are not configured so trying to save state should fail.
        vm.save_state().unwrap_err();

        let (_, vm, _mem) = setup_vm_with_memory(0x1000);
        vm.setup_irqchip().unwrap();

        let vm_state = vm.save_state().unwrap();
        assert_eq!(
            vm_state.pitstate.flags | KVM_PIT_SPEAKER_DUMMY,
            KVM_PIT_SPEAKER_DUMMY
        );
        assert_eq!(vm_state.clock.flags & KVM_CLOCK_TSC_STABLE, 0);
        assert_eq!(vm_state.pic_master.chip_id, KVM_IRQCHIP_PIC_MASTER);
        assert_eq!(vm_state.pic_slave.chip_id, KVM_IRQCHIP_PIC_SLAVE);
        assert_eq!(vm_state.ioapic.chip_id, KVM_IRQCHIP_IOAPIC);

        let (_, mut vm, _mem) = setup_vm_with_memory(0x1000);
        vm.setup_irqchip().unwrap();

        vm.restore_state(&vm_state).unwrap();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_vm_save_restore_state_bad_irqchip() {
        use kvm_bindings::KVM_NR_IRQCHIPS;

        let (_, vm, _mem) = setup_vm_with_memory(0x1000);
        vm.setup_irqchip().unwrap();
        let mut vm_state = vm.save_state().unwrap();

        let (_, mut vm, _mem) = setup_vm_with_memory(0x1000);
        vm.setup_irqchip().unwrap();

        // Try to restore an invalid PIC Master chip ID
        let orig_master_chip_id = vm_state.pic_master.chip_id;
        vm_state.pic_master.chip_id = KVM_NR_IRQCHIPS;
        vm.restore_state(&vm_state).unwrap_err();
        vm_state.pic_master.chip_id = orig_master_chip_id;

        // Try to restore an invalid PIC Slave chip ID
        let orig_slave_chip_id = vm_state.pic_slave.chip_id;
        vm_state.pic_slave.chip_id = KVM_NR_IRQCHIPS;
        vm.restore_state(&vm_state).unwrap_err();
        vm_state.pic_slave.chip_id = orig_slave_chip_id;

        // Try to restore an invalid IOPIC chip ID
        vm_state.ioapic.chip_id = KVM_NR_IRQCHIPS;
        vm.restore_state(&vm_state).unwrap_err();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_vmstate_serde() {
        let mut snapshot_data = vec![0u8; 10000];

        let (_, mut vm, _) = setup_vm_with_memory(0x1000);
        vm.setup_irqchip().unwrap();
        let state = vm.save_state().unwrap();
        Snapshot::serialize(&mut snapshot_data.as_mut_slice(), &state).unwrap();
        let restored_state: VmState = Snapshot::deserialize(&mut snapshot_data.as_slice()).unwrap();

        vm.restore_state(&restored_state).unwrap();
    }
}
