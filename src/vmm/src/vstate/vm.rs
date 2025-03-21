// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

use kvm_bindings::{KVM_MEM_LOG_DIRTY_PAGES, kvm_userspace_memory_region};
use kvm_ioctls::VmFd;
use vmm_sys_util::eventfd::EventFd;

use crate::Vcpu;
pub use crate::arch::{ArchVm as Vm, ArchVmError, VmState};
use crate::logger::info;
use crate::vstate::memory::{Address, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};
use crate::vstate::vcpu::VcpuError;

/// Architecture independent parts of a VM.
#[derive(Debug)]
pub struct VmCommon {
    /// The KVM file descriptor used to access this Vm.
    pub fd: VmFd,
    max_memslots: usize,
    guest_memory: Option<GuestMemoryMmap>,
}

/// Errors associated with the wrappers over KVM ioctls.
/// Needs `rustfmt::skip` to make multiline comments work
#[rustfmt::skip]
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum VmError {
    /// Cannot set the memory regions: {0}
    SetUserMemoryRegion(kvm_ioctls::Error),
    /// Failed to create VM: {0}
    CreateVm(kvm_ioctls::Error),
    /// {0}
    Arch(#[from] ArchVmError),
    /// Error during eventfd operations: {0}
    EventFd(std::io::Error),
    /// Failed to create vcpu: {0}
    CreateVcpu(VcpuError),
    /// The number of configured slots is bigger than the maximum reported by KVM
    NotEnoughMemorySlots,
}

/// Contains Vm functions that are usable across CPU architectures
impl Vm {
    /// Create a KVM VM
    pub fn create_common(kvm: &crate::vstate::kvm::Kvm) -> Result<VmCommon, VmError> {
        // It is known that KVM_CREATE_VM occasionally fails with EINTR on heavily loaded machines
        // with many VMs.
        //
        // The behavior itself that KVM_CREATE_VM can return EINTR is intentional. This is because
        // the KVM_CREATE_VM path includes mm_take_all_locks() that is CPU intensive and all CPU
        // intensive syscalls should check for pending signals and return EINTR immediately to allow
        // userland to remain interactive.
        // https://lists.nongnu.org/archive/html/qemu-devel/2014-01/msg01740.html
        //
        // However, it is empirically confirmed that, even though there is no pending signal,
        // KVM_CREATE_VM returns EINTR.
        // https://lore.kernel.org/qemu-devel/8735e0s1zw.wl-maz@kernel.org/
        //
        // To mitigate it, QEMU does an infinite retry on EINTR that greatly improves reliabiliy:
        // - https://github.com/qemu/qemu/commit/94ccff133820552a859c0fb95e33a539e0b90a75
        // - https://github.com/qemu/qemu/commit/bbde13cd14ad4eec18529ce0bf5876058464e124
        //
        // Similarly, we do retries up to 5 times. Although Firecracker clients are also able to
        // retry, they have to start Firecracker from scratch. Doing retries in Firecracker makes
        // recovery faster and improves reliability.
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt = 1;
        let fd = loop {
            match kvm.fd.create_vm() {
                Ok(fd) => break fd,
                Err(e) if e.errno() == libc::EINTR && attempt < MAX_ATTEMPTS => {
                    info!("Attempt #{attempt} of KVM_CREATE_VM returned EINTR");
                    // Exponential backoff (1us, 2us, 4us, and 8us => 15us in total)
                    std::thread::sleep(std::time::Duration::from_micros(2u64.pow(attempt - 1)));
                }
                Err(e) => return Err(VmError::CreateVm(e)),
            }

            attempt += 1;
        };

        Ok(VmCommon {
            fd,
            max_memslots: kvm.max_nr_memslots(),
            guest_memory: None,
        })
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

    /// Initializes the guest memory.
    pub fn memory_init(&mut self, guest_mem: GuestMemoryMmap) -> Result<(), VmError> {
        if guest_mem.num_regions() > self.common.max_memslots {
            return Err(VmError::NotEnoughMemorySlots);
        }

        self.set_kvm_memory_regions(&guest_mem)?;
        self.common.guest_memory = Some(guest_mem);

        Ok(())
    }

    pub(crate) fn set_kvm_memory_regions(
        &self,
        guest_mem: &GuestMemoryMmap,
    ) -> Result<(), VmError> {
        guest_mem
            .iter()
            .zip(0u32..)
            .try_for_each(|(region, slot)| {
                let flags = if region.bitmap().is_some() {
                    KVM_MEM_LOG_DIRTY_PAGES
                } else {
                    0
                };

                let memory_region = kvm_userspace_memory_region {
                    slot,
                    guest_phys_addr: region.start_addr().raw_value(),
                    memory_size: region.len(),
                    userspace_addr: region.as_ptr() as u64,
                    flags,
                };

                // SAFETY: Safe because the fd is a valid KVM file descriptor.
                unsafe { self.fd().set_user_memory_region(memory_region) }
            })
            .map_err(VmError::SetUserMemoryRegion)?;
        Ok(())
    }

    /// Gets a reference to the kvm file descriptor owned by this VM.
    pub fn fd(&self) -> &VmFd {
        &self.common.fd
    }

    /// Gets a reference to this [`Vm`]'s [`GuestMemoryMmap`] object
    pub fn guest_memory(&self) -> &GuestMemoryMmap {
        self.common
            .guest_memory
            .as_ref()
            .expect("attempt to access guest memory before calling Vm::memory_init()")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use vm_memory::GuestAddress;
    use vm_memory::mmap::MmapRegionBuilder;

    use super::*;
    use crate::test_utils::single_region_mem;
    use crate::utils::mib_to_bytes;
    use crate::vstate::kvm::Kvm;
    use crate::vstate::memory::GuestRegionMmap;

    // Auxiliary function being used throughout the tests.
    pub(crate) fn setup_vm() -> (Kvm, Vm) {
        let kvm = Kvm::new(vec![]).expect("Cannot create Kvm");
        let vm = Vm::new(&kvm).expect("Cannot create new vm");
        (kvm, vm)
    }

    // Auxiliary function being used throughout the tests.
    pub(crate) fn setup_vm_with_memory(mem_size: usize) -> (Kvm, Vm) {
        let (kvm, mut vm) = setup_vm();
        let gm = single_region_mem(mem_size);
        vm.memory_init(gm).unwrap();
        (kvm, vm)
    }

    #[test]
    fn test_new() {
        // Testing with a valid /dev/kvm descriptor.
        let kvm = Kvm::new(vec![]).expect("Cannot create Kvm");
        Vm::new(&kvm).unwrap();
    }

    #[test]
    fn test_vm_memory_init() {
        let (_, mut vm) = setup_vm();
        // Create valid memory region and test that the initialization is successful.
        let gm = single_region_mem(0x1000);
        vm.memory_init(gm).unwrap();
    }

    #[test]
    fn test_set_kvm_memory_regions() {
        let (_, vm) = setup_vm();

        let gm = single_region_mem(0x1000);
        let res = vm.set_kvm_memory_regions(&gm);
        res.unwrap();

        // Trying to set a memory region with a size that is not a multiple of GUEST_PAGE_SIZE
        // will result in error.
        let gm = single_region_mem(0x10);
        let res = vm.set_kvm_memory_regions(&gm);
        assert_eq!(
            res.unwrap_err().to_string(),
            "Cannot set the memory regions: Invalid argument (os error 22)"
        );
    }

    #[test]
    fn test_too_many_regions() {
        let (kvm, mut vm) = setup_vm();
        let max_nr_regions = kvm.max_nr_memslots();

        let mut gm = GuestMemoryMmap::default();
        // SAFETY: valid mmap parameters
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                0x1000,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };

        assert_ne!(ptr, libc::MAP_FAILED);

        for i in 0..max_nr_regions {
            // SAFETY: we assert above that the ptr is valid, and the size matches what we passed to
            // mmap
            let region = unsafe {
                MmapRegionBuilder::new(0x1000)
                    .with_raw_mmap_pointer(ptr.cast())
                    .build()
                    .unwrap()
            };

            let region = GuestRegionMmap::new(region, GuestAddress(i as u64 * 0x1000)).unwrap();

            gm = gm.insert_region(Arc::new(region)).unwrap();
        }

        vm.memory_init(gm.clone()).unwrap();

        let mut vm = Vm::new(&kvm).unwrap();

        // SAFETY: we assert above that the ptr is valid, and the size matches what we passed to
        // mmap
        let region = unsafe {
            MmapRegionBuilder::new(0x1000)
                .with_raw_mmap_pointer(ptr.cast())
                .build()
                .unwrap()
        };

        let region =
            GuestRegionMmap::new(region, GuestAddress(max_nr_regions as u64 * 0x1000)).unwrap();

        let err = vm
            .memory_init(gm.insert_region(Arc::new(region)).unwrap())
            .unwrap_err();

        assert!(matches!(err, VmError::NotEnoughMemorySlots), "{:?}", err);
    }

    #[test]
    fn test_create_vcpus() {
        let vcpu_count = 2;
        let (_, mut vm) = setup_vm_with_memory(mib_to_bytes(128));

        let (vcpu_vec, _) = vm.create_vcpus(vcpu_count).unwrap();

        assert_eq!(vcpu_vec.len(), vcpu_count as usize);
    }
}
