// Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Module containing various structs, ioctls, constants and functions related to guest_memfd support
//!
//! Intended only a PoC, and to keep the changes to Firecracker somewhat centralized

#![allow(missing_docs)]

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use kvm_bindings::KVMIO;
use kvm_ioctls::VmFd;
use vm_memory::{Address, GuestMemory, GuestMemoryRegion};
use vmm_sys_util::ioctl::ioctl_with_ref;
use vmm_sys_util::{ioctl_iow_nr, ioctl_iowr_nr};
use vmm_sys_util::ioctl_ioc_nr;
use vmm_sys_util::syscall::SyscallReturnCode;
use crate::builder::StartMicrovmError;
use crate::{Vm, Vmm};
use crate::vstate::memory::{GuestRegionMmap, MemoryError};
use crate::vstate::vm::VmError;


/// VM type that supports guest private memory
pub const KVM_X86_SW_PROTECTED_VM: u64 = 1;

#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Copy, Clone, Default)]
struct kvm_create_guest_memfd {
    size: u64,
    flags: u64,
    reserved: [u64; 6],
}

// ioctl to create a guest_memfd. Has to be executed on a vm fd, to which
// the returned guest_memfd will be bound (e.g. it can only be used to back
// memory in that specific VM).
ioctl_iowr_nr!(KVM_CREATE_GUEST_MEMFD, KVMIO, 0xd4, kvm_create_guest_memfd);

#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Copy, Clone, Default, Debug)]
struct kvm_userspace_memory_region2 {
    slot: u32,
    flags: u32,
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
    guest_memfd_offset: u64,
    guest_memfd: u32,
    pad1: u32,
    pad2: [u64; 14],
}

// VM ioctl for registering memory regions that have a guest_memfd associated with them
ioctl_iow_nr!(
    KVM_SET_USER_MEMORY_REGION2,
    KVMIO,
    0x49,
    kvm_userspace_memory_region2
);

/// Flag passed to [`KVM_SET_USER_MEMORY_REGION2`] to indicate that a region supports
/// private memory.
const KVM_MEM_PRIVATE: u32 = 1 << 2;

/// Bitflag to mark a specific (range of) page frame(s) as private
const KVM_MEMORY_ATTRIBUTE_PRIVATE: u64 = 1u64 << 3;

#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Copy, Clone, Default)]
struct kvm_memory_attributes {
    address: u64,
    size: u64,
    attributes: u64,
    flags: u64,
}

// VM ioctl used to mark guest page frames as shared/private
ioctl_iow_nr!(
    KVM_SET_MEMORY_ATTRIBUTES,
    KVMIO,
    0xd2,
    kvm_memory_attributes
);

/// Creates a `guest_memfd` of the given size in bytes, tied to the given VM.
pub fn create_guest_memfd(vm: &VmFd, size: u64) -> Result<File, MemoryError> {
    let guest_memfd = SyscallReturnCode(unsafe {
        ioctl_with_ref(
            vm,
            KVM_CREATE_GUEST_MEMFD(),
            &kvm_create_guest_memfd {
                size,
                flags: 0,
                ..Default::default()
            },
        )
    })
        .into_result()
        .map_err(MemoryError::GuestMemfd)?;

    unsafe { Ok(File::from_raw_fd(guest_memfd)) }
}

impl Vm {
    pub fn set_userspace_memory_region2(&self, slot: u32, region: &GuestRegionMmap, guest_memfd: &File) -> Result<(), VmError> {
        // Set the "userspace_addr" to an mmap of the guest_memfd. Since the guest_memfd is mapped
        // into host userspace, this will trick KVM into gup-ing guest_memfd whenever it thinks
        // that it is accessing normal guest memory (as KVM's guest memory accessor functions
        // are not enlightened about the possibility of guest_memfd existing, and will always
        // gup via userspace_addr).
        let memory_region = kvm_userspace_memory_region2 {
            slot,
            guest_phys_addr: region.start_addr().raw_value(),
            memory_size: region.len(),
            userspace_addr: region.as_ptr() as u64,
            guest_memfd_offset: region.start_addr().raw_value(),
            guest_memfd: guest_memfd.as_raw_fd() as u32,
            flags: KVM_MEM_PRIVATE,
            ..Default::default()
        };

        if unsafe {
            ioctl_with_ref(self.fd(), KVM_SET_USER_MEMORY_REGION2(), &memory_region)
        } < 0
        {
            Err(VmError::SetUserMemoryRegion(kvm_ioctls::Error::last()))
        } else {
            Ok(())
        }
    }
}

impl Vmm {
    pub fn set_guest_memory_private(&self) -> Result<(), StartMicrovmError> {
       for region in self.guest_memory.iter() {
            let attributes = kvm_memory_attributes {
                address: region.start_addr().raw_value(),
                size: region.len(),
                attributes: KVM_MEMORY_ATTRIBUTE_PRIVATE,
                ..Default::default()
            };

            unsafe {
                SyscallReturnCode(ioctl_with_ref(
                    self.vm.fd(),
                    KVM_SET_MEMORY_ATTRIBUTES(),
                    &attributes,
                ))
                    .into_empty_result()
                    .map_err(MemoryError::SetMemoryAttributes)
                    .map_err(StartMicrovmError::GuestMemory)?
            }
        }

        Ok(())
    }
}