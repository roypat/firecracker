// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};

use kvm_bindings::{kvm_create_guest_memfd, kvm_memory_attributes, kvm_userspace_memory_region2, KVM_MEMORY_ATTRIBUTE_PRIVATE, KVM_MEM_GUEST_MEMFD, KVM_MEM_LOG_DIRTY_PAGES, KVM_X86_SW_PROTECTED_VM};
use kvm_ioctls::VmFd;
use vmm_sys_util::eventfd::EventFd;

#[cfg(target_arch = "x86_64")]
use crate::utils::u64_to_usize;
use crate::vstate::kvm::Kvm;
use crate::vstate::memory::{
    Address, GuestMemory, GuestMemoryMmap, GuestMemoryRegion, GuestRegionMmap,
};

#[cfg(target_arch = "x86_64")]
#[path = "x86_64.rs"]
mod arch;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod arch;

pub use arch::{ArchVm as Vm, ArchVmError, VmState};

use crate::vstate::vcpu::VcpuError;
use crate::Vcpu;

/// Errors associated with the wrappers over KVM ioctls.
/// Needs `rustfmt::skip` to make multiline comments work
#[rustfmt::skip]
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum VmError {
    /// Cannot set the memory regions: {0}
    SetUserMemoryRegion(kvm_ioctls::Error),
    /// Cannot open the VM file descriptor: {0}
    VmFd(kvm_ioctls::Error),
    /// Cannot configure the microvm: {0}
    VmSetup(kvm_ioctls::Error),
    /// {0}
    Arch(#[from] ArchVmError),
    /// Error during eventfd operations: {0}
    EventFd(std::io::Error),
    /// Failed to create vcpu: {0}
    CreateVcpu(VcpuError),
    /// Failed to create guest_memfd: {0}
    CreateGuestMemfd(kvm_ioctls::Error),
    /// Failed to set memory attributes of guest_memfd-backed memory to private: {0}
    SetMemoryAttributes(kvm_ioctls::Error),
}

/// Contains Vm functions that are usable across CPU architectures
impl Vm {
    /// Create a new `Vm` struct.
    pub fn new(kvm: &Kvm) -> Result<Self, VmError> {
        let fd = kvm.fd.create_vm_with_type(KVM_X86_SW_PROTECTED_VM as u64).map_err(VmError::VmFd)?;

        Vm::arch_create(kvm, fd).map_err(VmError::Arch)
    }

    /// Creates the specified number of [`Vcpu`]s.
    ///
    /// The returned [`EventFd`] is written to whenever any of the vcpus exit.
    pub fn create_vcpus(&mut self, vcpu_count: u8) -> Result<(Vec<Vcpu>, EventFd), VmError> {
        self.arch_pre_create_vcpus(vcpu_count)?;

        let exit_evt = EventFd::new(libc::EFD_NONBLOCK).map_err(VmError::EventFd)?;

        let mut vcpus = Vec::with_capacity(vcpu_count as usize);
        for cpu_idx in 0..vcpu_count {
            let exit_evt = exit_evt.try_clone().map_err(VmError::EventFd)?;
            let vcpu = Vcpu::new(cpu_idx, self, exit_evt).map_err(VmError::CreateVcpu)?;
            vcpus.push(vcpu);
        }

        self.arch_post_create_vcpus(vcpu_count)?;

        Ok((vcpus, exit_evt))
    }

    /// Create a guest_memfd of the specified size
    pub fn create_guest_memfd(&self, size_mib: usize) -> Result<File, VmError> {
        let guest_memfd = self
            .fd
            .create_guest_memfd(kvm_create_guest_memfd {
                size: (size_mib as u64) << 20,
                ..Default::default()
            })
            .map_err(VmError::CreateGuestMemfd)?;

        // SAFETY: `create_guest_memfd` only returns `Ok(raw_fd)` if the ioctl was actually
        // successful, so we know we have a valid fd here.
        unsafe { Ok(File::from_raw_fd(guest_memfd)) }
    }

    /// Initializes the guest memory.
    pub fn memory_init(
        &self,
        shared_mem: &GuestMemoryMmap,
        private_mem: Option<&GuestMemoryMmap>,
    ) -> Result<(), VmError> {
        shared_mem
            .iter()
            .map(kvmify)
            .chain(
                private_mem
                    .iter()
                    .flat_map(|private_mem| private_mem.iter())
                    .map(|region| {
                        let mut kvm_region = kvmify(region);
                        let file_offset = region.file_offset().unwrap();

                        kvm_region.flags |= KVM_MEM_GUEST_MEMFD;
                        kvm_region.guest_memfd = file_offset.file().as_raw_fd() as _;
                        kvm_region.guest_memfd_offset = file_offset.start();

                        kvm_region
                    }),
            )
            .zip(0u32..)
            .map(|(mut kvm_region, slot)| {
                kvm_region.slot = slot;
                kvm_region
            })
            .try_for_each(|kvm_region| unsafe { self.fd.set_user_memory_region2(kvm_region) })
            .map_err(VmError::SetUserMemoryRegion)?;

        if let Some(private_mem) = private_mem {
            for priv_region in private_mem.iter() {
                self.fd
                    .set_memory_attributes(kvm_memory_attributes {
                        address: priv_region.start_addr().raw_value(),
                        size: priv_region.size() as _,
                        attributes: KVM_MEMORY_ATTRIBUTE_PRIVATE as _,
                        ..Default::default()
                    })
                    .map_err(VmError::SetMemoryAttributes)?;
            }
        }

        #[cfg(target_arch = "x86_64")]
        self.fd
            .set_tss_address(u64_to_usize(crate::arch::x86_64::layout::KVM_TSS_ADDRESS))
            .map_err(VmError::VmSetup)?;

        Ok(())
    }

    /// Gets a reference to the kvm file descriptor owned by this VM.
    pub fn fd(&self) -> &VmFd {
        &self.fd
    }
}

fn kvmify(region: &GuestRegionMmap) -> kvm_userspace_memory_region2 {
    let flags = if region.bitmap().is_some() {
        KVM_MEM_LOG_DIRTY_PAGES
    } else {
        0
    };

    kvm_userspace_memory_region2 {
        flags,
        guest_phys_addr: region.start_addr().raw_value(),
        memory_size: region.len(),
        userspace_addr: region.as_ptr() as u64,
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_utils::single_region_mem;
    use crate::vstate::memory::GuestMemoryMmap;

    // Auxiliary function being used throughout the tests.
    pub(crate) fn setup_vm() -> (Kvm, Vm) {
        let kvm = Kvm::new(vec![]).expect("Cannot create Kvm");
        let vm = Vm::new(&kvm).expect("Cannot create new vm");
        (kvm, vm)
    }

    // Auxiliary function being used throughout the tests.
    pub(crate) fn setup_vm_with_memory(mem_size: usize) -> (Kvm, Vm, GuestMemoryMmap) {
        let (kvm, vm) = setup_vm();
        let shared = single_region_mem(mem_size);
        vm.memory_init(&shared, None).unwrap();
        (kvm, vm, shared)
    }

    #[test]
    fn test_new() {
        // Testing with a valid /dev/kvm descriptor.
        let kvm = Kvm::new(vec![]).expect("Cannot create Kvm");
        Vm::new(&kvm).unwrap();
    }

    #[test]
    fn test_vm_memory_init() {
        let (_, vm) = setup_vm();
        // Create valid memory region and test that the initialization is successful.
        let shared = single_region_mem(0x1000);
        vm.memory_init(&shared, None).unwrap();
    }

    #[test]
    fn test_create_vcpus() {
        let vcpu_count = 2;
        let (_, mut vm, _) = setup_vm_with_memory(128 << 20);

        let (vcpu_vec, _) = vm.create_vcpus(vcpu_count).unwrap();

        assert_eq!(vcpu_vec.len(), vcpu_count as usize);
    }
}
