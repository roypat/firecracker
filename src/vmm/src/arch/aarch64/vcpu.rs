// Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

use std::fmt::{Debug, Write};
use std::mem::offset_of;
use std::path::PathBuf;

use kvm_bindings::*;
use kvm_ioctls::{VcpuExit, VcpuFd, VmFd};
use serde::{Deserialize, Serialize};

use super::get_fdt_addr;
use super::regs::*;
use crate::arch::EntryPoint;
use crate::arch::aarch64::kvm::OptionalCapabilities;
use crate::arch::aarch64::regs::{Aarch64RegisterVec, KVM_REG_ARM64_SVE_VLS};
use crate::cpu_config::aarch64::custom_cpu_template::VcpuFeatures;
use crate::cpu_config::templates::CpuConfiguration;
use crate::logger::{IncMetric, METRICS, error};
use crate::vcpu::{VcpuConfig, VcpuError};
use crate::vstate::memory::{Address, GuestMemoryMmap};
use crate::vstate::vcpu::VcpuEmulation;
use crate::vstate::vm::Vm;

/// Errors thrown while setting aarch64 registers.
#[derive(Debug, PartialEq, Eq, thiserror::Error, displaydoc::Display)]
pub enum VcpuArchError {
    /// Failed to get register {0}: {1}
    GetOneReg(u64, kvm_ioctls::Error),
    /// Failed to set register {0}: {1}
    SetOneReg(u64, kvm_ioctls::Error),
    /// Failed to retrieve list of registers: {0}
    GetRegList(kvm_ioctls::Error),
    /// Failed to get multiprocessor state: {0}
    GetMp(kvm_ioctls::Error),
    /// Failed to set multiprocessor state: {0}
    SetMp(kvm_ioctls::Error),
    /// Failed FamStructWrapper operation: {0}
    Fam(vmm_sys_util::fam::Error),
    /// {0}
    GetMidrEl1(String),
}

/// Extract the Manufacturer ID from the host.
/// The ID is found between bits 24-31 of MIDR_EL1 register.
pub fn get_manufacturer_id_from_host() -> Result<u32, VcpuArchError> {
    let midr_el1_path =
        &PathBuf::from("/sys/devices/system/cpu/cpu0/regs/identification/midr_el1".to_string());

    let midr_el1 = std::fs::read_to_string(midr_el1_path).map_err(|err| {
        VcpuArchError::GetMidrEl1(format!("Failed to get MIDR_EL1 from host path: {err}"))
    })?;
    let midr_el1_trimmed = midr_el1.trim_end().trim_start_matches("0x");
    let manufacturer_id = u32::from_str_radix(midr_el1_trimmed, 16).map_err(|err| {
        VcpuArchError::GetMidrEl1(format!("Invalid MIDR_EL1 found on host: {err}",))
    })?;

    Ok(manufacturer_id >> 24)
}

/// Configure relevant boot registers for a given vCPU.
///
/// # Arguments
///
/// * `cpu_id` - Index of current vcpu.
/// * `boot_ip` - Starting instruction pointer.
/// * `mem` - Reserved DRAM for current VM.
pub fn setup_boot_regs(
    vcpufd: &VcpuFd,
    cpu_id: u8,
    boot_ip: u64,
    mem: &GuestMemoryMmap,
    optional_capabilities: &OptionalCapabilities,
) -> Result<(), VcpuArchError> {
    let kreg_off = offset_of!(kvm_regs, regs);

    // Get the register index of the PSTATE (Processor State) register.
    let pstate = offset_of!(user_pt_regs, pstate) + kreg_off;
    let id = arm64_core_reg_id!(KVM_REG_SIZE_U64, pstate);
    vcpufd
        .set_one_reg(id, &PSTATE_FAULT_BITS_64.to_le_bytes())
        .map_err(|err| VcpuArchError::SetOneReg(id, err))?;

    // Other vCPUs are powered off initially awaiting PSCI wakeup.
    if cpu_id == 0 {
        // Setting the PC (Processor Counter) to the current program address (kernel address).
        let pc = offset_of!(user_pt_regs, pc) + kreg_off;
        let id = arm64_core_reg_id!(KVM_REG_SIZE_U64, pc);
        vcpufd
            .set_one_reg(id, &boot_ip.to_le_bytes())
            .map_err(|err| VcpuArchError::SetOneReg(id, err))?;

        // Last mandatory thing to set -> the address pointing to the FDT (also called DTB).
        // "The device tree blob (dtb) must be placed on an 8-byte boundary and must
        // not exceed 2 megabytes in size." -> https://www.kernel.org/doc/Documentation/arm64/booting.txt.
        // We are choosing to place it the end of DRAM. See `get_fdt_addr`.
        let regs0 = offset_of!(user_pt_regs, regs) + kreg_off;
        let id = arm64_core_reg_id!(KVM_REG_SIZE_U64, regs0);
        vcpufd
            .set_one_reg(id, &get_fdt_addr(mem).to_le_bytes())
            .map_err(|err| VcpuArchError::SetOneReg(id, err))?;

        // Reset the physical counter for the guest. This way we avoid guest reading
        // host physical counter.
        // Resetting KVM_REG_ARM_PTIMER_CNT for single vcpu is enough because there is only
        // one timer struct with offsets per VM.
        // Because the access to KVM_REG_ARM_PTIMER_CNT is only present starting 6.4 kernel,
        // we only do the reset if KVM_CAP_COUNTER_OFFSET is present as it was added
        // in the same patch series as the ability to set the KVM_REG_ARM_PTIMER_CNT register.
        // Path series which introduced the needed changes:
        // https://lore.kernel.org/all/20230330174800.2677007-1-maz@kernel.org/
        // Note: the value observed by the guest will still be above 0, because there is a delta
        // time between this resetting and first call to KVM_RUN.
        if optional_capabilities.counter_offset {
            vcpufd
                .set_one_reg(KVM_REG_ARM_PTIMER_CNT, &[0; 8])
                .map_err(|err| VcpuArchError::SetOneReg(id, err))?;
        }
    }
    Ok(())
}

/// Saves the states of the system registers into `state`.
///
/// # Arguments
///
/// * `regs` - Input/Output vector of registers.
pub fn get_all_registers(
    vcpufd: &VcpuFd,
    state: &mut Aarch64RegisterVec,
) -> Result<(), VcpuArchError> {
    get_registers(vcpufd, &get_all_registers_ids(vcpufd)?, state)
}

/// Saves states of registers into `state`.
///
/// # Arguments
///
/// * `ids` - Slice of registers ids to save.
/// * `regs` - Input/Output vector of registers.
pub fn get_registers(
    vcpufd: &VcpuFd,
    ids: &[u64],
    regs: &mut Aarch64RegisterVec,
) -> Result<(), VcpuArchError> {
    let mut big_reg = [0_u8; 256];
    for id in ids.iter() {
        let reg_size = vcpufd
            .get_one_reg(*id, &mut big_reg)
            .map_err(|e| VcpuArchError::GetOneReg(*id, e))?;
        let reg_ref = Aarch64RegisterRef::new(*id, &big_reg[0..reg_size]);
        regs.push(reg_ref);
    }
    Ok(())
}

/// Returns all registers ids, including core and system
pub fn get_all_registers_ids(vcpufd: &VcpuFd) -> Result<Vec<u64>, VcpuArchError> {
    // Call KVM_GET_REG_LIST to get all registers available to the guest. For ArmV8 there are
    // less than 500 registers expected, resize to the reported size when necessary.
    let mut reg_list = RegList::new(500).map_err(VcpuArchError::Fam)?;

    match vcpufd.get_reg_list(&mut reg_list) {
        Ok(_) => Ok(reg_list.as_slice().to_vec()),
        Err(e) => match e.errno() {
            libc::E2BIG => {
                // resize and retry.
                let size: usize = reg_list
                    .as_fam_struct_ref()
                    .n
                    .try_into()
                    // Safe to unwrap as Firecracker only targets 64-bit machines.
                    .unwrap();
                reg_list = RegList::new(size).map_err(VcpuArchError::Fam)?;
                vcpufd
                    .get_reg_list(&mut reg_list)
                    .map_err(VcpuArchError::GetRegList)?;

                Ok(reg_list.as_slice().to_vec())
            }
            _ => Err(VcpuArchError::GetRegList(e)),
        },
    }
}

/// Set the state of one system register.
///
/// # Arguments
///
/// * `reg` - Register to be set.
pub fn set_register(vcpufd: &VcpuFd, reg: Aarch64RegisterRef) -> Result<(), VcpuArchError> {
    vcpufd
        .set_one_reg(reg.id, reg.as_slice())
        .map_err(|e| VcpuArchError::SetOneReg(reg.id, e))?;
    Ok(())
}

/// Get the multistate processor.
///
/// # Arguments
///
/// * `vcpu` - Structure for the VCPU that holds the VCPU's fd.
pub fn get_mpstate(vcpufd: &VcpuFd) -> Result<kvm_mp_state, VcpuArchError> {
    vcpufd.get_mp_state().map_err(VcpuArchError::GetMp)
}

/// Set the state of the system registers.
///
/// # Arguments
///
/// * `vcpu` - Structure for the VCPU that holds the VCPU's fd.
/// * `state` - Structure for returning the state of the system registers.
pub fn set_mpstate(vcpufd: &VcpuFd, state: kvm_mp_state) -> Result<(), VcpuArchError> {
    vcpufd.set_mp_state(state).map_err(VcpuArchError::SetMp)
}

/// Errors associated with the wrappers over KVM ioctls.
#[derive(Debug, PartialEq, Eq, thiserror::Error, displaydoc::Display)]
pub enum KvmVcpuError {
    /// Error configuring the vcpu registers: {0}
    ConfigureRegisters(VcpuArchError),
    /// Error creating vcpu: {0}
    CreateVcpu(kvm_ioctls::Error),
    /// Failed to dump CPU configuration: {0}
    DumpCpuConfig(VcpuArchError),
    /// Error getting the vcpu preferred target: {0}
    GetPreferredTarget(kvm_ioctls::Error),
    /// Error initializing the vcpu: {0}
    Init(kvm_ioctls::Error),
    /// Error applying template: {0}
    ApplyCpuTemplate(VcpuArchError),
    /// Failed to restore the state of the vcpu: {0}
    RestoreState(VcpuArchError),
    /// Failed to save the state of the vcpu: {0}
    SaveState(VcpuArchError),
}

/// Error type for [`KvmVcpu::configure`].
pub type KvmVcpuConfigureError = KvmVcpuError;

/// A wrapper around creating and using a kvm aarch64 vcpu.
#[derive(Debug)]
pub struct KvmVcpu {
    /// Index of vcpu.
    pub index: u8,
    /// KVM vcpu fd.
    pub fd: VcpuFd,
    /// Vcpu peripherals, such as buses
    pub peripherals: Peripherals,
    kvi: kvm_vcpu_init,
}

/// Vcpu peripherals
#[derive(Default, Debug)]
pub struct Peripherals {
    /// mmio bus.
    pub mmio_bus: Option<crate::devices::Bus>,
}

impl KvmVcpu {
    /// Constructs a new kvm vcpu with arch specific functionality.
    ///
    /// # Arguments
    ///
    /// * `index` - Represents the 0-based CPU index between [0, max vcpus).
    /// * `vm` - The vm to which this vcpu will get attached.
    pub fn new(index: u8, vm: &Vm) -> Result<Self, KvmVcpuError> {
        let kvm_vcpu = vm
            .fd()
            .create_vcpu(index.into())
            .map_err(KvmVcpuError::CreateVcpu)?;

        let mut kvi = Self::default_kvi(vm.fd())?;
        // Secondary vcpus must be powered off for boot process.
        if 0 < index {
            kvi.features[0] |= 1 << KVM_ARM_VCPU_POWER_OFF;
        }

        Ok(KvmVcpu {
            index,
            fd: kvm_vcpu,
            peripherals: Default::default(),
            kvi,
        })
    }

    /// Read the MPIDR - Multiprocessor Affinity Register.
    pub fn get_mpidr(&self) -> Result<u64, VcpuArchError> {
        // MPIDR register is 64 bit wide on aarch64
        let mut mpidr = [0_u8; 8];
        match self.fd.get_one_reg(MPIDR_EL1, &mut mpidr) {
            Err(err) => Err(VcpuArchError::GetOneReg(MPIDR_EL1, err)),
            Ok(_) => Ok(u64::from_le_bytes(mpidr)),
        }
    }

    /// Configures an aarch64 specific vcpu for booting Linux.
    ///
    /// # Arguments
    ///
    /// * `guest_mem` - The guest memory used by this microvm.
    /// * `kernel_entry_point` - Specifies the boot protocol and offset from `guest_mem` at which
    ///   the kernel starts.
    /// * `vcpu_config` - The vCPU configuration.
    pub fn configure(
        &mut self,
        guest_mem: &GuestMemoryMmap,
        kernel_entry_point: EntryPoint,
        vcpu_config: &VcpuConfig,
        optional_capabilities: &OptionalCapabilities,
    ) -> Result<(), KvmVcpuError> {
        for reg in vcpu_config.cpu_config.regs.iter() {
            self.fd.set_one_reg(reg.id, reg.as_slice()).map_err(|err| {
                KvmVcpuError::ApplyCpuTemplate(VcpuArchError::SetOneReg(reg.id, err))
            })?;
        }

        setup_boot_regs(
            &self.fd,
            self.index,
            kernel_entry_point.entry_addr.raw_value(),
            guest_mem,
            optional_capabilities,
        )
        .map_err(KvmVcpuError::ConfigureRegisters)?;

        Ok(())
    }

    /// Initializes an aarch64 specific vcpu for booting Linux.
    ///
    /// # Arguments
    ///
    /// * `vm_fd` - The kvm `VmFd` for this microvm.
    pub fn init(&mut self, vcpu_features: &[VcpuFeatures]) -> Result<(), KvmVcpuError> {
        for feature in vcpu_features.iter() {
            let index = feature.index as usize;
            self.kvi.features[index] = feature.bitmap.apply(self.kvi.features[index]);
        }

        self.init_vcpu()?;
        self.finalize_vcpu()?;

        Ok(())
    }

    /// Creates default kvi struct based on vcpu index.
    pub fn default_kvi(vm_fd: &VmFd) -> Result<kvm_vcpu_init, KvmVcpuError> {
        let mut kvi = kvm_vcpu_init::default();
        // This reads back the kernel's preferred target type.
        vm_fd
            .get_preferred_target(&mut kvi)
            .map_err(KvmVcpuError::GetPreferredTarget)?;
        // We already checked that the capability is supported.
        kvi.features[0] |= 1 << KVM_ARM_VCPU_PSCI_0_2;

        Ok(kvi)
    }

    /// Save the KVM internal state.
    pub fn save_state(&self) -> Result<VcpuState, KvmVcpuError> {
        let mut state = VcpuState {
            mp_state: get_mpstate(&self.fd).map_err(KvmVcpuError::SaveState)?,
            ..Default::default()
        };
        get_all_registers(&self.fd, &mut state.regs).map_err(KvmVcpuError::SaveState)?;
        state.mpidr = self.get_mpidr().map_err(KvmVcpuError::SaveState)?;

        state.kvi = self.kvi;
        // We don't save power off state in a snapshot, because
        // it was only needed during uVM boot process.
        // When uVM is restored, the kernel has already passed
        // the boot state and turned secondary vcpus on.
        state.kvi.features[0] &= !(1 << KVM_ARM_VCPU_POWER_OFF);

        Ok(state)
    }

    /// Use provided state to populate KVM internal state.
    pub fn restore_state(&mut self, state: &VcpuState) -> Result<(), KvmVcpuError> {
        self.kvi = state.kvi;

        self.init_vcpu()?;

        // If KVM_REG_ARM64_SVE_VLS is present it needs to
        // be set before vcpu is finalized.
        if let Some(sve_vls_reg) = state
            .regs
            .iter()
            .find(|reg| reg.id == KVM_REG_ARM64_SVE_VLS)
        {
            set_register(&self.fd, sve_vls_reg).map_err(KvmVcpuError::RestoreState)?;
        }

        self.finalize_vcpu()?;

        // KVM_REG_ARM64_SVE_VLS needs to be skipped after vcpu is finalized.
        // If it is present it is handled in the code above.
        for reg in state
            .regs
            .iter()
            .filter(|reg| reg.id != KVM_REG_ARM64_SVE_VLS)
        {
            set_register(&self.fd, reg).map_err(KvmVcpuError::RestoreState)?;
        }
        set_mpstate(&self.fd, state.mp_state).map_err(KvmVcpuError::RestoreState)?;
        Ok(())
    }

    /// Dumps CPU configuration.
    pub fn dump_cpu_config(&self) -> Result<CpuConfiguration, KvmVcpuError> {
        let reg_list = get_all_registers_ids(&self.fd).map_err(KvmVcpuError::DumpCpuConfig)?;

        let mut regs = Aarch64RegisterVec::default();
        get_registers(&self.fd, &reg_list, &mut regs).map_err(KvmVcpuError::DumpCpuConfig)?;

        Ok(CpuConfiguration { regs })
    }
    /// Initializes internal vcpufd.
    fn init_vcpu(&self) -> Result<(), KvmVcpuError> {
        self.fd.vcpu_init(&self.kvi).map_err(KvmVcpuError::Init)?;
        Ok(())
    }

    /// Checks for SVE feature and calls `vcpu_finalize` if
    /// it is enabled.
    fn finalize_vcpu(&self) -> Result<(), KvmVcpuError> {
        if (self.kvi.features[0] & (1 << KVM_ARM_VCPU_SVE)) != 0 {
            // KVM_ARM_VCPU_SVE has value 4 so casting to i32 is safe.
            #[allow(clippy::cast_possible_wrap)]
            let feature = KVM_ARM_VCPU_SVE as i32;
            self.fd.vcpu_finalize(&feature).unwrap();
        }
        Ok(())
    }
}

impl Peripherals {
    /// Runs the vCPU in KVM context and handles the kvm exit reason.
    ///
    /// Returns error or enum specifying whether emulation was handled or interrupted.
    pub fn run_arch_emulation(&self, exit: VcpuExit) -> Result<VcpuEmulation, VcpuError> {
        METRICS.vcpu.failures.inc();
        // TODO: Are we sure we want to finish running a vcpu upon
        // receiving a vm exit that is not necessarily an error?
        error!("Unexpected exit reason on vcpu run: {:?}", exit);
        Err(VcpuError::UnhandledKvmExit(format!("{:?}", exit)))
    }
}

/// Structure holding VCPU kvm state.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct VcpuState {
    /// Multiprocessing state.
    pub mp_state: kvm_mp_state,
    /// Vcpu registers.
    pub regs: Aarch64RegisterVec,
    /// We will be using the mpidr for passing it to the VmState.
    /// The VmState will give this away for saving restoring the icc and redistributor
    /// registers.
    pub mpidr: u64,
    /// kvi states for vcpu initialization.
    pub kvi: kvm_vcpu_init,
}

impl Debug for VcpuState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "kvm_mp_state: {:#x}", self.mp_state.mp_state)?;
        writeln!(f, "mpidr: {:#x}", self.mpidr)?;
        for reg in self.regs.iter() {
            writeln!(
                f,
                "{:#x} 0x{}",
                reg.id,
                reg.as_slice()
                    .iter()
                    .rev()
                    .fold(String::new(), |mut output, b| {
                        let _ = write!(output, "{b:x}");
                        output
                    })
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::undocumented_unsafe_blocks)]
    use std::os::unix::io::AsRawFd;

    use kvm_bindings::{KVM_ARM_VCPU_PSCI_0_2, KVM_REG_SIZE_U64};
    use vm_memory::GuestAddress;

    use super::*;
    use crate::arch::BootProtocol;
    use crate::arch::aarch64::layout;
    use crate::arch::aarch64::regs::Aarch64RegisterRef;
    use crate::cpu_config::aarch64::CpuConfiguration;
    use crate::cpu_config::templates::RegisterValueFilter;
    use crate::test_utils::arch_mem;
    use crate::vcpu::VcpuConfig;
    use crate::vstate::kvm::Kvm;
    use crate::vstate::memory::GuestMemoryMmap;
    use crate::vstate::vm::Vm;
    use crate::vstate::vm::tests::setup_vm_with_memory;

    fn setup_vcpu(mem_size: usize) -> (Kvm, Vm, KvmVcpu, GuestMemoryMmap) {
        let (kvm, mut vm, vm_mem) = setup_vm_with_memory(mem_size);
        let mut vcpu = KvmVcpu::new(0, &vm).unwrap();
        vcpu.init(&[]).unwrap();
        vm.setup_irqchip(1).unwrap();

        (kvm, vm, vcpu, vm_mem)
    }

    #[test]
    fn test_create_vcpu() {
        let (_, vm, _) = setup_vm_with_memory(0x1000);

        unsafe { libc::close(vm.fd().as_raw_fd()) };

        let err = KvmVcpu::new(0, &vm);
        assert_eq!(
            err.err().unwrap().to_string(),
            "Error creating vcpu: Bad file descriptor (os error 9)".to_string()
        );

        // dropping vm would double close the gic fd, so leak it
        std::mem::forget(vm);
    }

    #[test]
    fn test_configure_vcpu() {
        let (kvm, _, mut vcpu, vm_mem) = setup_vcpu(0x10000);
        let optional_capabilities = kvm.optional_capabilities();

        let vcpu_config = VcpuConfig {
            vcpu_count: 1,
            smt: false,
            cpu_config: CpuConfiguration::default(),
        };

        vcpu.configure(
            &vm_mem,
            EntryPoint {
                entry_addr: GuestAddress(crate::arch::get_kernel_start()),
                protocol: BootProtocol::LinuxBoot,
            },
            &vcpu_config,
            &optional_capabilities,
        )
        .unwrap();

        unsafe { libc::close(vcpu.fd.as_raw_fd()) };

        let err = vcpu.configure(
            &vm_mem,
            EntryPoint {
                entry_addr: GuestAddress(crate::arch::get_kernel_start()),
                protocol: BootProtocol::LinuxBoot,
            },
            &vcpu_config,
            &optional_capabilities,
        );
        assert_eq!(
            err.unwrap_err(),
            KvmVcpuError::ConfigureRegisters(VcpuArchError::SetOneReg(
                0x6030000000100042,
                kvm_ioctls::Error::new(9)
            ))
        );

        // dropping vcpu would double close the gic fd, so leak it
        std::mem::forget(vcpu);
    }

    #[test]
    fn test_init_vcpu() {
        let (_, mut vm, _) = setup_vm_with_memory(0x1000);
        let mut vcpu = KvmVcpu::new(0, &vm).unwrap();
        vm.setup_irqchip(1).unwrap();

        // KVM_ARM_VCPU_PSCI_0_2 is set by default.
        // we check if we can remove it.
        let vcpu_features = vec![VcpuFeatures {
            index: 0,
            bitmap: RegisterValueFilter {
                filter: 1 << KVM_ARM_VCPU_PSCI_0_2,
                value: 0,
            },
        }];
        vcpu.init(&vcpu_features).unwrap();
        assert!((vcpu.kvi.features[0] & (1 << KVM_ARM_VCPU_PSCI_0_2)) == 0)
    }

    #[test]
    fn test_vcpu_save_restore_state() {
        let (_, mut vm, _) = setup_vm_with_memory(0x1000);
        let mut vcpu = KvmVcpu::new(0, &vm).unwrap();
        vm.setup_irqchip(1).unwrap();

        // Calling KVM_GET_REGLIST before KVM_VCPU_INIT will result in error.
        let res = vcpu.save_state();
        assert!(matches!(
            res.unwrap_err(),
            KvmVcpuError::SaveState(VcpuArchError::GetRegList(_))
        ));

        // Try to restore the register using a faulty state.
        let mut faulty_vcpu_state = VcpuState::default();

        // Try faulty kvi state
        let res = vcpu.restore_state(&faulty_vcpu_state);
        assert!(matches!(res.unwrap_err(), KvmVcpuError::Init(_)));

        // Try faulty vcpu regs
        faulty_vcpu_state.kvi = KvmVcpu::default_kvi(vm.fd()).unwrap();
        let mut regs = Aarch64RegisterVec::default();
        let mut reg = Aarch64RegisterRef::new(KVM_REG_SIZE_U64, &[0; 8]);
        reg.id = 0;
        regs.push(reg);
        faulty_vcpu_state.regs = regs;
        let res = vcpu.restore_state(&faulty_vcpu_state);
        assert!(matches!(
            res.unwrap_err(),
            KvmVcpuError::RestoreState(VcpuArchError::SetOneReg(0, _))
        ));

        vcpu.init(&[]).unwrap();
        let state = vcpu.save_state().expect("Cannot save state of vcpu");
        assert!(!state.regs.is_empty());
        vcpu.restore_state(&state)
            .expect("Cannot restore state of vcpu");
    }

    #[test]
    fn test_dump_cpu_config_before_init() {
        // Test `dump_cpu_config()` before `KVM_VCPU_INIT`.
        //
        // This should fail with ENOEXEC.
        // https://elixir.bootlin.com/linux/v5.10.176/source/arch/arm64/kvm/arm.c#L1165
        let (_, mut vm, _) = setup_vm_with_memory(0x1000);
        let vcpu = KvmVcpu::new(0, &vm).unwrap();
        vm.setup_irqchip(1).unwrap();

        vcpu.dump_cpu_config().unwrap_err();
    }

    #[test]
    fn test_dump_cpu_config_after_init() {
        // Test `dump_cpu_config()` after `KVM_VCPU_INIT`.
        let (_, mut vm, _) = setup_vm_with_memory(0x1000);
        let mut vcpu = KvmVcpu::new(0, &vm).unwrap();
        vm.setup_irqchip(1).unwrap();
        vcpu.init(&[]).unwrap();

        vcpu.dump_cpu_config().unwrap();
    }

    #[test]
    fn test_setup_non_boot_vcpu() {
        let (_, vm, _) = setup_vm_with_memory(0x1000);
        let mut vcpu1 = KvmVcpu::new(0, &vm).unwrap();
        vcpu1.init(&[]).unwrap();
        let mut vcpu2 = KvmVcpu::new(1, &vm).unwrap();
        vcpu2.init(&[]).unwrap();
    }

    #[test]
    fn test_get_valid_regs() {
        // Test `get_regs()` with valid register IDs.
        // - X0: 0x6030 0000 0010 0000
        // - X1: 0x6030 0000 0010 0002
        let (_, _, vcpu, _) = setup_vcpu(0x10000);
        let reg_list = Vec::<u64>::from([0x6030000000100000, 0x6030000000100002]);
        get_registers(&vcpu.fd, &reg_list, &mut Aarch64RegisterVec::default()).unwrap();
    }

    #[test]
    fn test_get_invalid_regs() {
        // Test `get_regs()` with invalid register IDs.
        let (_, _, vcpu, _) = setup_vcpu(0x10000);
        let reg_list = Vec::<u64>::from([0x6030000000100001, 0x6030000000100003]);
        get_registers(&vcpu.fd, &reg_list, &mut Aarch64RegisterVec::default()).unwrap_err();
    }

    #[test]
    fn test_setup_regs() {
        let kvm = Kvm::new(vec![]).unwrap();
        let vm = kvm.fd.create_vm().unwrap();
        let vcpu = vm.create_vcpu(0).unwrap();
        let mem = arch_mem(layout::FDT_MAX_SIZE + 0x1000);
        let optional_capabilities = kvm.optional_capabilities();

        let res = setup_boot_regs(&vcpu, 0, 0x0, &mem, &optional_capabilities);
        assert!(matches!(
            res.unwrap_err(),
            VcpuArchError::SetOneReg(0x6030000000100042, _)
        ));

        let mut kvi: kvm_bindings::kvm_vcpu_init = kvm_bindings::kvm_vcpu_init::default();
        vm.get_preferred_target(&mut kvi).unwrap();
        vcpu.vcpu_init(&kvi).unwrap();

        setup_boot_regs(&vcpu, 0, 0x0, &mem, &optional_capabilities).unwrap();

        // Check that the register is reset on compatible kernels.
        // Because there is a delta in time between we reset the register and time we
        // read it, we cannot compare with 0. Instead we compare it with meaningfully
        // small value.
        if optional_capabilities.counter_offset {
            let mut reg_bytes = [0_u8; 8];
            vcpu.get_one_reg(SYS_CNTPCT_EL0, &mut reg_bytes).unwrap();
            let counter_value = u64::from_le_bytes(reg_bytes);

            // We are reading the SYS_CNTPCT_EL0 right after resetting it.
            // If reset did happen successfully, the value should be quite small when we read it.
            // If the reset did not happen, the value will be same as on the host and it surely
            // will be more that `max_value`. Measurements show that usually value is close
            // to 1000. Use bigger `max_value` just in case.
            let max_value = 10_000;

            assert!(counter_value < max_value);
        }
    }

    #[test]
    fn test_read_mpidr() {
        let (_, _, vcpu, _) = setup_vcpu(0x10000);

        // Must fail when vcpu is not initialized yet.
        let res = vcpu.get_mpidr();
        assert!(matches!(
            res.unwrap_err(),
            VcpuArchError::GetOneReg(MPIDR_EL1, _)
        ));

        vcpu.init_vcpu().unwrap();
        assert_eq!(vcpu.get_mpidr().unwrap(), 0x8000_0000);
    }

    #[test]
    fn test_get_set_regs() {
        let kvm = Kvm::new(vec![]).unwrap();
        let vm = kvm.fd.create_vm().unwrap();
        let vcpu = vm.create_vcpu(0).unwrap();
        let mut kvi: kvm_bindings::kvm_vcpu_init = kvm_bindings::kvm_vcpu_init::default();
        vm.get_preferred_target(&mut kvi).unwrap();

        // Must fail when vcpu is not initialized yet.
        let mut regs = Aarch64RegisterVec::default();
        let res = get_all_registers(&vcpu, &mut regs);
        assert!(matches!(res.unwrap_err(), VcpuArchError::GetRegList(_)));

        vcpu.vcpu_init(&kvi).unwrap();
        get_all_registers(&vcpu, &mut regs).unwrap();
        for reg in regs.iter() {
            set_register(&vcpu, reg).unwrap();
        }
    }

    #[test]
    fn test_mpstate() {
        use std::os::unix::io::AsRawFd;

        let kvm = Kvm::new(vec![]).unwrap();
        let vm = kvm.fd.create_vm().unwrap();
        let vcpu = vm.create_vcpu(0).unwrap();
        let mut kvi: kvm_bindings::kvm_vcpu_init = kvm_bindings::kvm_vcpu_init::default();
        vm.get_preferred_target(&mut kvi).unwrap();

        let res = get_mpstate(&vcpu);
        set_mpstate(&vcpu, res.unwrap()).unwrap();

        unsafe { libc::close(vcpu.as_raw_fd()) };

        let res = get_mpstate(&vcpu);
        assert!(matches!(res, Err(VcpuArchError::GetMp(_))), "{:?}", res);

        let res = set_mpstate(&vcpu, kvm_mp_state::default());
        assert!(matches!(res, Err(VcpuArchError::SetMp(_))), "{:?}", res);

        // dropping vcpu would double close the fd, so leak it
        std::mem::forget(vcpu);
    }
}
