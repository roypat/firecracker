// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::os::fd::AsRawFd;
use std::ptr::null_mut;
use std::sync::Arc;

use kvm_bindings::{KVM_MEM_GUEST_MEMFD, KVM_MEM_LOG_DIRTY_PAGES, kvm_userspace_memory_region2};
use serde::{Deserialize, Serialize};
pub use vm_memory::bitmap::{AtomicBitmap, BS, Bitmap, BitmapSlice};
pub use vm_memory::mmap::MmapRegionBuilder;
use vm_memory::mmap::{MmapRegionError, NewBitmap};
use vm_memory::volatile_memory::compute_offset;
pub use vm_memory::{
    Address, ByteValued, Bytes, FileOffset, GuestAddress, GuestMemory, GuestMemoryRegion,
    GuestUsize, MemoryRegionAddress, MmapRegion, address,
};
use vm_memory::{
    Error as VmMemoryError, GuestMemoryError, ReadVolatile, VolatileMemoryError, VolatileSlice,
    WriteVolatile,
};
use vmm_sys_util::errno;

use crate::DirtyBitmap;
use crate::utils::{get_page_size, u64_to_usize};
use crate::vmm_config::machine_config::HugePageConfig;

/// Type of GuestMemoryMmap.
pub type GuestMemoryMmap = crate::vm_memory_vendored::GuestRegionCollection<KvmRegion>;
/// Type of GuestRegionMmap.
pub type GuestRegionMmap = vm_memory::GuestRegionMmap<Option<AtomicBitmap>>;
/// Type of GuestMmapRegion.
pub type GuestMmapRegion = vm_memory::MmapRegion<Option<AtomicBitmap>>;

/// Errors associated with dumping guest memory to file.
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum MemoryError {
    /// Cannot fetch system's page size: {0}
    PageSize(errno::Error),
    /// Cannot dump memory: {0}
    WriteMemory(GuestMemoryError),
    /// Cannot create mmap region: {0}
    MmapRegionError(MmapRegionError),
    /// Cannot create guest memory: {0}
    VmMemoryError(VmMemoryError),
    /// Cannot create memfd: {0}
    Memfd(memfd::Error),
    /// Cannot resize memfd file: {0}
    MemfdSetLen(std::io::Error),
    /// Total sum of memory regions exceeds largest possible file offset
    OffsetTooLarge,
    /// Error calling mmap: {0}
    Mmap(std::io::Error),
}

/// Newtype that implements [`ReadVolatile`] and [`WriteVolatile`] if `T` implements `Read` or
/// `Write` respectively, by reading/writing using a bounce buffer, and memcpy-ing into the
/// [`VolatileSlice`].
///
/// Bounce buffers are allocated on the heap, as on-stack bounce buffers could cause stack
/// overflows. If `N == 0` then bounce buffers will be allocated on demand.
#[derive(Debug)]
pub struct MaybeBounce<T, const N: usize = 0> {
    pub(crate) target: T,
    persistent_buffer: Option<Box<[u8; N]>>,
}

impl<T> MaybeBounce<T, 0> {
    /// Creates a new `MaybeBounce` that always allocates a bounce
    /// buffer on-demand
    pub fn new(target: T, should_bounce: bool) -> Self {
        MaybeBounce::new_persistent(target, should_bounce)
    }
}

impl<T, const N: usize> MaybeBounce<T, N> {
    /// Creates a new `MaybeBounce` that uses a persistent, fixed size bounce buffer
    /// of size `N`. If a read/write request exceeds the size of this bounce buffer, it
    /// is split into multiple, `<= N`-size read/writes.
    pub fn new_persistent(target: T, should_bounce: bool) -> Self {
        let mut bounce = MaybeBounce {
            target,
            persistent_buffer: None,
        };

        if should_bounce {
            bounce.activate()
        }

        bounce
    }

    /// Activates this [`MaybeBounce`] to start doing reads/writes via a bounce buffer,
    /// which is allocated on the heap by this function (e.g. if `activate()` is never called,
    /// no bounce buffer is ever allocated).
    pub fn activate(&mut self) {
        self.persistent_buffer = Some(vec![0u8; N].into_boxed_slice().try_into().unwrap())
    }
}

// FIXME: replace AsFd with ReadVolatile once &File: ReadVolatile in vm-memory.
impl<T: ReadVolatile, const N: usize> ReadVolatile for MaybeBounce<T, N> {
    fn read_volatile<B: BitmapSlice>(
        &mut self,
        buf: &mut VolatileSlice<B>,
    ) -> Result<usize, VolatileMemoryError> {
        if let Some(ref mut persistent) = self.persistent_buffer {
            let mut bbuf = (N == 0).then(|| vec![0u8; buf.len()]);
            let bbuf = bbuf.as_deref_mut().unwrap_or(persistent.as_mut_slice());

            let mut buf = buf.offset(0)?;
            let mut total = 0;
            while !buf.is_empty() {
                let how_much = buf.len().min(bbuf.len());
                let n = self
                    .target
                    .read_volatile(&mut VolatileSlice::from(&mut bbuf[..how_much]))?;
                buf.copy_from(&bbuf[..n]);

                buf = buf.offset(n)?;
                total += n;

                if n < how_much {
                    break;
                }
            }

            Ok(total)
        } else {
            self.target.read_volatile(buf)
        }
    }
}

impl<T: WriteVolatile, const N: usize> WriteVolatile for MaybeBounce<T, N> {
    fn write_volatile<B: BitmapSlice>(
        &mut self,
        buf: &VolatileSlice<B>,
    ) -> Result<usize, VolatileMemoryError> {
        if let Some(ref mut persistent) = self.persistent_buffer {
            let mut bbuf = (N == 0).then(|| vec![0u8; buf.len()]);
            let bbuf = bbuf.as_deref_mut().unwrap_or(persistent.as_mut_slice());

            let mut buf = buf.offset(0)?;
            let mut total = 0;
            while !buf.is_empty() {
                let how_much = buf.copy_to(bbuf);
                let n = self
                    .target
                    .write_volatile(&VolatileSlice::from(&mut bbuf[..how_much]))?;
                buf = buf.offset(n)?;
                total += n;

                if n < how_much {
                    break;
                }
            }

            Ok(total)
        } else {
            self.target.write_volatile(buf)
        }
    }
}

impl<R: Read, const N: usize> Read for MaybeBounce<R, N> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.target.read(buf)
    }
}

impl<W: Write, const N: usize> Write for MaybeBounce<W, N> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.target.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.target.flush()
    }
}

impl<S: Seek, const N: usize> Seek for MaybeBounce<S, N> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.target.seek(pos)
    }
}

/// A memory region, described in terms of `kvm_userspace_memory_region`
#[derive(Debug)]
pub struct KvmRegion {
    region: kvm_userspace_memory_region2,
    bitmap: Option<AtomicBitmap>,
    file_offset: Option<FileOffset>,
}

impl KvmRegion {
    /// Constructs a new [`KvmRegion`] from the given [`kvm_userspace_memory_region`] and bitmap.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that as long as this `KvmRegion` object is alive,
    /// `kvm_region.userspace_addr as *mut u8` is valid for reads and writes of length
    /// `kvm_region.memory_size`.
    pub unsafe fn new(
        region: kvm_userspace_memory_region2,
        bitmap: Option<AtomicBitmap>,
        file_offset: Option<FileOffset>,
    ) -> Self {
        KvmRegion {
            region,
            bitmap,
            file_offset,
        }
    }

    pub(crate) fn from_mmap_region(
        region: GuestRegionMmap,
        slot: u32,
        guest_memfd: Option<FileOffset>,
    ) -> Self {
        let region = ManuallyDrop::new(region);
        let mut flags = 0;
        if region.bitmap().is_some() {
            flags |= KVM_MEM_LOG_DIRTY_PAGES;
        }
        if guest_memfd.is_some() {
            flags |= KVM_MEM_GUEST_MEMFD;
        }

        #[allow(clippy::cast_sign_loss)]
        let (guest_memfd, guest_memfd_offset) = guest_memfd
            .map(|fo| (fo.file().as_raw_fd() as u32, fo.start()))
            .unwrap_or((0, 0));

        // SAFETY: `GuestRegionMmap` is essentially a fat pointer, and ensures that
        // region.as_ptr() is valid for reads and writes of length region.len(),
        // and by placing our region into a `ManuallyDrop` we ensure that its `Drop`
        // impl won't run and free the memory away from underneath us.
        unsafe {
            Self::new(
                kvm_userspace_memory_region2 {
                    slot,
                    flags,
                    guest_phys_addr: region.start_addr().0,
                    memory_size: region.len(),
                    userspace_addr: region.as_ptr() as u64,
                    guest_memfd,
                    guest_memfd_offset,
                    ..Default::default()
                },
                region.bitmap().clone(),
                region.file_offset().cloned(),
            )
        }
    }

    pub(crate) fn inner(&self) -> &kvm_userspace_memory_region2 {
        &self.region
    }
}

#[allow(clippy::cast_possible_wrap)]
#[allow(clippy::cast_possible_truncation)]
impl GuestMemoryRegion for KvmRegion {
    type B = Option<AtomicBitmap>;

    fn len(&self) -> GuestUsize {
        self.region.memory_size
    }

    fn start_addr(&self) -> GuestAddress {
        GuestAddress(self.region.guest_phys_addr)
    }

    fn bitmap(&self) -> &Self::B {
        &self.bitmap
    }

    fn get_host_address(
        &self,
        addr: MemoryRegionAddress,
    ) -> vm_memory::guest_memory::Result<*mut u8> {
        self.check_address(addr)
            .ok_or(vm_memory::guest_memory::Error::InvalidBackendAddress)
            .map(|addr| {
                (self.region.userspace_addr as *mut u8).wrapping_offset(addr.raw_value() as isize)
            })
    }

    fn file_offset(&self) -> Option<&FileOffset> {
        self.file_offset.as_ref()
    }

    fn get_slice(
        &self,
        offset: MemoryRegionAddress,
        count: usize,
    ) -> vm_memory::guest_memory::Result<VolatileSlice<BS<Self::B>>> {
        let offset = u64_to_usize(offset.0);
        let end_addr = compute_offset(offset, count)? as u64;
        if end_addr > self.len() {
            return Err(vm_memory::guest_memory::Error::InvalidBackendAddress);
        }

        // SAFETY: Safe because we checked that offset + count was within our range and we only
        // ever hand out volatile accessors.
        unsafe {
            Ok(VolatileSlice::with_bitmap(
                (self.region.userspace_addr as *mut u8).add(offset),
                count,
                self.bitmap.slice_at(offset),
                None,
            ))
        }
    }
}

/// Creates a `Vec` of `GuestRegionMmap` with the given configuration
pub fn create(
    regions: impl Iterator<Item = (GuestAddress, usize)>,
    mmap_flags: libc::c_int,
    file: Option<File>,
    track_dirty_pages: bool,
    mut offset: u64,
) -> Result<Vec<GuestRegionMmap>, MemoryError> {
    let file = file.map(Arc::new);
    regions
        .map(|(start, size)| {
            let mut builder = MmapRegionBuilder::new_with_bitmap(
                size,
                track_dirty_pages.then(|| AtomicBitmap::with_len(size)),
            );

            // when computing offset below we ensure it fits into i64
            #[allow(clippy::cast_possible_wrap)]
            let (fd, fd_off) = if let Some(ref file) = file {
                let file_offset = FileOffset::from_arc(Arc::clone(file), offset);

                builder = builder.with_file_offset(file_offset);

                (file.as_raw_fd(), offset as libc::off_t)
            } else {
                (-1, 0)
            };

            // SAFETY: the arguments to mmap cannot cause any memory unsafety in the rust sense
            let ptr = unsafe {
                libc::mmap(
                    null_mut(),
                    size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_NORESERVE | mmap_flags,
                    fd,
                    fd_off,
                )
            };

            if ptr == libc::MAP_FAILED {
                return Err(MemoryError::Mmap(std::io::Error::last_os_error()));
            }

            // SAFETY: we check above that mmap succeeded, and the size we passed to builder is the
            // same as the size of the mmap area.
            let builder = unsafe { builder.with_raw_mmap_pointer(ptr.cast()) };

            offset = match offset.checked_add(size as u64) {
                None => return Err(MemoryError::OffsetTooLarge),
                Some(new_off) if new_off >= i64::MAX as u64 => {
                    return Err(MemoryError::OffsetTooLarge);
                }
                Some(new_off) => new_off,
            };

            GuestRegionMmap::new(
                builder.build().map_err(MemoryError::MmapRegionError)?,
                start,
            )
            .map_err(MemoryError::VmMemoryError)
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Creates a GuestMemoryMmap with `size` in MiB backed by a memfd.
pub fn file_shared(
    file: File,
    regions: impl Iterator<Item = (GuestAddress, usize)>,
    track_dirty_pages: bool,
    huge_pages: HugePageConfig,
) -> Result<Vec<GuestRegionMmap>, MemoryError> {
    create(
        regions,
        libc::MAP_SHARED | huge_pages.mmap_flags(),
        Some(file),
        track_dirty_pages,
        0,
    )
}

/// Creates a GuestMemoryMmap from raw regions.
pub fn anonymous(
    regions: impl Iterator<Item = (GuestAddress, usize)>,
    track_dirty_pages: bool,
    huge_pages: HugePageConfig,
) -> Result<Vec<GuestRegionMmap>, MemoryError> {
    create(
        regions,
        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | huge_pages.mmap_flags(),
        None,
        track_dirty_pages,
        0,
    )
}

/// Creates a GuestMemoryMmap given a `file` containing the data
/// and a `state` containing mapping information.
pub fn file_private(
    file: File,
    regions: impl Iterator<Item = (GuestAddress, usize)>,
    track_dirty_pages: bool,
    offset: u64,
) -> Result<Vec<GuestRegionMmap>, MemoryError> {
    create(
        regions,
        libc::MAP_PRIVATE,
        Some(file),
        track_dirty_pages,
        offset,
    )
}

/// Defines the interface for snapshotting memory.
pub trait GuestMemoryExtension
where
    Self: Sized,
{
    /// Describes GuestMemoryMmap through a GuestMemoryState struct.
    fn describe(&self) -> GuestMemoryState;

    /// Mark memory range as dirty
    fn mark_dirty(&self, addr: GuestAddress, len: usize);

    /// Dumps all contents of GuestMemoryMmap to a writer.
    fn dump<T: WriteVolatile>(&self, writer: &mut T) -> Result<(), MemoryError>;

    /// Dumps all pages of GuestMemoryMmap present in `dirty_bitmap` to a writer.
    fn dump_dirty<T: WriteVolatile + std::io::Seek>(
        &self,
        writer: &mut T,
        dirty_bitmap: &DirtyBitmap,
    ) -> Result<(), MemoryError>;

    /// Resets all the memory region bitmaps
    fn reset_dirty(&self);

    /// Store the dirty bitmap in internal store
    fn store_dirty_bitmap(&self, dirty_bitmap: &DirtyBitmap, page_size: usize);
}

/// State of a guest memory region saved to file/buffer.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestMemoryRegionState {
    // This should have been named `base_guest_addr` since it's _guest_ addr, but for
    // backward compatibility we have to keep this name. At least this comment should help.
    /// Base GuestAddress.
    pub base_address: u64,
    /// Region size.
    pub size: usize,
}

/// Describes guest memory regions and their snapshot file mappings.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestMemoryState {
    /// List of regions.
    pub regions: Vec<GuestMemoryRegionState>,
}

impl GuestMemoryState {
    /// Turns this [`GuestMemoryState`] into a description of guest memory regions as understood
    /// by the creation functions of [`GuestMemoryExtensions`]
    pub fn regions(&self) -> impl Iterator<Item = (GuestAddress, usize)> + '_ {
        self.regions
            .iter()
            .map(|region| (GuestAddress(region.base_address), region.size))
    }
}

impl GuestMemoryExtension for GuestMemoryMmap {
    /// Describes GuestMemoryMmap through a GuestMemoryState struct.
    fn describe(&self) -> GuestMemoryState {
        let mut guest_memory_state = GuestMemoryState::default();
        self.iter().for_each(|region| {
            guest_memory_state.regions.push(GuestMemoryRegionState {
                base_address: region.start_addr().0,
                size: u64_to_usize(region.len()),
            });
        });
        guest_memory_state
    }

    /// Mark memory range as dirty
    fn mark_dirty(&self, addr: GuestAddress, len: usize) {
        let _ = self.try_access(len, addr, |_total, count, caddr, region| {
            if let Some(bitmap) = region.bitmap() {
                bitmap.mark_dirty(u64_to_usize(caddr.0), count);
            }
            Ok(count)
        });
    }

    /// Dumps all contents of GuestMemoryMmap to a writer.
    fn dump<T: WriteVolatile>(&self, writer: &mut T) -> Result<(), MemoryError> {
        self.iter()
            .try_for_each(|region| Ok(writer.write_all_volatile(&region.as_volatile_slice()?)?))
            .map_err(MemoryError::WriteMemory)
    }

    /// Dumps all pages of GuestMemoryMmap present in `dirty_bitmap` to a writer.
    fn dump_dirty<T: WriteVolatile + std::io::Seek>(
        &self,
        writer: &mut T,
        dirty_bitmap: &DirtyBitmap,
    ) -> Result<(), MemoryError> {
        let mut writer_offset = 0;
        let page_size = get_page_size().map_err(MemoryError::PageSize)?;

        let write_result = self.iter().try_for_each(|region| {
            let kvm_bitmap = dirty_bitmap.get(&region.inner().slot).unwrap();
            let firecracker_bitmap = region.bitmap();
            let mut write_size = 0;
            let mut dirty_batch_start: u64 = 0;

            for (i, v) in kvm_bitmap.iter().enumerate() {
                for j in 0..64 {
                    let is_kvm_page_dirty = ((v >> j) & 1u64) != 0u64;
                    let page_offset = ((i * 64) + j) * page_size;
                    let is_firecracker_page_dirty = firecracker_bitmap.dirty_at(page_offset);

                    if is_kvm_page_dirty || is_firecracker_page_dirty {
                        // We are at the start of a new batch of dirty pages.
                        if write_size == 0 {
                            // Seek forward over the unmodified pages.
                            writer
                                .seek(SeekFrom::Start(writer_offset + page_offset as u64))
                                .unwrap();
                            dirty_batch_start = page_offset as u64;
                        }
                        write_size += page_size;
                    } else if write_size > 0 {
                        // We are at the end of a batch of dirty pages.
                        writer.write_all_volatile(
                            &region
                                .get_slice(MemoryRegionAddress(dirty_batch_start), write_size)?,
                        )?;

                        write_size = 0;
                    }
                }
            }

            if write_size > 0 {
                writer.write_all_volatile(
                    &region.get_slice(MemoryRegionAddress(dirty_batch_start), write_size)?,
                )?;
            }
            writer_offset += region.len();

            Ok(())
        });

        if write_result.is_err() {
            self.store_dirty_bitmap(dirty_bitmap, page_size);
        } else {
            self.reset_dirty();
        }

        write_result.map_err(MemoryError::WriteMemory)
    }

    /// Resets all the memory region bitmaps
    fn reset_dirty(&self) {
        self.iter().for_each(|region| {
            if let Some(bitmap) = region.bitmap() {
                bitmap.reset();
            }
        })
    }

    /// Stores the dirty bitmap inside into the internal bitmap
    fn store_dirty_bitmap(&self, dirty_bitmap: &DirtyBitmap, page_size: usize) {
        self.iter().for_each(|region| {
            let kvm_bitmap = dirty_bitmap.get(&region.inner().slot).unwrap();
            let firecracker_bitmap = region.bitmap();

            for (i, v) in kvm_bitmap.iter().enumerate() {
                for j in 0..64 {
                    let is_kvm_page_dirty = ((v >> j) & 1u64) != 0u64;

                    if is_kvm_page_dirty {
                        let page_offset = ((i * 64) + j) * page_size;

                        firecracker_bitmap.mark_dirty(page_offset, 1)
                    }
                }
            }
        });
    }
}

/// Creates a memfd of the given size and huge pages configuration
pub fn create_memfd(
    mem_size: u64,
    hugetlb_size: Option<memfd::HugetlbSize>,
) -> Result<memfd::Memfd, MemoryError> {
    // Create a memfd.
    let opts = memfd::MemfdOptions::default()
        .hugetlb(hugetlb_size)
        .allow_sealing(true);
    let mem_file = opts.create("guest_mem").map_err(MemoryError::Memfd)?;

    // Resize to guest mem size.
    mem_file
        .as_file()
        .set_len(mem_size)
        .map_err(MemoryError::MemfdSetLen)?;

    // Add seals to prevent further resizing.
    let mut seals = memfd::SealsHashSet::new();
    seals.insert(memfd::FileSeal::SealShrink);
    seals.insert(memfd::FileSeal::SealGrow);
    mem_file.add_seals(&seals).map_err(MemoryError::Memfd)?;

    // Prevent further sealing changes.
    mem_file
        .add_seal(memfd::FileSeal::SealSeal)
        .map_err(MemoryError::Memfd)?;

    Ok(mem_file)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::undocumented_unsafe_blocks)]

    use std::collections::HashMap;
    use std::io::{Read, Seek};
    use std::os::fd::AsFd;

    use itertools::Itertools;
    use vmm_sys_util::tempfile::TempFile;

    use super::*;
    use crate::snapshot::Snapshot;
    use crate::utils::{get_page_size, mib_to_bytes};

    fn kvmify(regions: Vec<GuestRegionMmap>) -> GuestMemoryMmap {
        GuestMemoryMmap::from_regions(
            regions
                .into_iter()
                .zip(0u32..) // assign dummy slots
                .map(|(region, slot)| KvmRegion::from_mmap_region(region, slot, None))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn test_anonymous() {
        for dirty_page_tracking in [true, false] {
            let region_size = 0x10000;
            let regions = vec![
                (GuestAddress(0x0), region_size),
                (GuestAddress(0x10000), region_size),
                (GuestAddress(0x20000), region_size),
                (GuestAddress(0x30000), region_size),
            ];

            let guest_memory = anonymous(
                regions.into_iter(),
                dirty_page_tracking,
                HugePageConfig::None,
            )
            .unwrap();
            guest_memory.iter().for_each(|region| {
                assert_eq!(region.bitmap().is_some(), dirty_page_tracking);
            });
        }
    }

    #[test]
    fn test_mark_dirty() {
        let page_size = get_page_size().unwrap();
        let region_size = page_size * 3;

        let regions = vec![
            (GuestAddress(0), region_size),                      // pages 0-2
            (GuestAddress(region_size as u64), region_size),     // pages 3-5
            (GuestAddress(region_size as u64 * 2), region_size), // pages 6-8
        ];
        let guest_memory =
            kvmify(anonymous(regions.into_iter(), true, HugePageConfig::None).unwrap());

        let dirty_map = [
            // page 0: not dirty
            (0, page_size, false),
            // pages 1-2: dirty range in one region
            (page_size, page_size * 2, true),
            // page 3: not dirty
            (page_size * 3, page_size, false),
            // pages 4-7: dirty range across 2 regions,
            (page_size * 4, page_size * 4, true),
            // page 8: not dirty
            (page_size * 8, page_size, false),
        ];

        // Mark dirty memory
        for (addr, len, dirty) in &dirty_map {
            if *dirty {
                guest_memory.mark_dirty(GuestAddress(*addr as u64), *len);
            }
        }

        // Check that the dirty memory was set correctly
        for (addr, len, dirty) in &dirty_map {
            guest_memory
                .try_access(
                    *len,
                    GuestAddress(*addr as u64),
                    |_total, count, caddr, region| {
                        let offset = usize::try_from(caddr.0).unwrap();
                        let bitmap = region.bitmap().as_ref().unwrap();
                        for i in offset..offset + count {
                            assert_eq!(bitmap.dirty_at(i), *dirty);
                        }
                        Ok(count)
                    },
                )
                .unwrap();
        }
    }

    fn check_serde<M: GuestMemoryExtension>(guest_memory: &M) {
        let mut snapshot_data = vec![0u8; 10000];
        let original_state = guest_memory.describe();
        Snapshot::serialize(&mut snapshot_data.as_mut_slice(), &original_state).unwrap();
        let restored_state = Snapshot::deserialize(&mut snapshot_data.as_slice()).unwrap();
        assert_eq!(original_state, restored_state);
    }

    #[test]
    fn test_serde() {
        let page_size = get_page_size().unwrap();
        let region_size = page_size * 3;

        // Test with a single region
        let guest_memory = kvmify(
            anonymous(
                [(GuestAddress(0), region_size)].into_iter(),
                false,
                HugePageConfig::None,
            )
            .unwrap(),
        );
        check_serde(&guest_memory);

        // Test with some regions
        let regions = vec![
            (GuestAddress(0), region_size),                      // pages 0-2
            (GuestAddress(region_size as u64), region_size),     // pages 3-5
            (GuestAddress(region_size as u64 * 2), region_size), // pages 6-8
        ];
        let guest_memory =
            kvmify(anonymous(regions.into_iter(), true, HugePageConfig::None).unwrap());
        check_serde(&guest_memory);
    }

    #[test]
    fn test_describe() {
        let page_size: usize = get_page_size().unwrap();

        // Two regions of one page each, with a one page gap between them.
        let mem_regions = [
            (GuestAddress(0), page_size),
            (GuestAddress(page_size as u64 * 2), page_size),
        ];
        let guest_memory =
            kvmify(anonymous(mem_regions.into_iter(), true, HugePageConfig::None).unwrap());

        let expected_memory_state = GuestMemoryState {
            regions: vec![
                GuestMemoryRegionState {
                    base_address: 0,
                    size: page_size,
                },
                GuestMemoryRegionState {
                    base_address: page_size as u64 * 2,
                    size: page_size,
                },
            ],
        };

        let actual_memory_state = guest_memory.describe();
        assert_eq!(expected_memory_state, actual_memory_state);

        // Two regions of three pages each, with a one page gap between them.
        let mem_regions = [
            (GuestAddress(0), page_size * 3),
            (GuestAddress(page_size as u64 * 4), page_size * 3),
        ];
        let guest_memory =
            kvmify(anonymous(mem_regions.into_iter(), true, HugePageConfig::None).unwrap());

        let expected_memory_state = GuestMemoryState {
            regions: vec![
                GuestMemoryRegionState {
                    base_address: 0,
                    size: page_size * 3,
                },
                GuestMemoryRegionState {
                    base_address: page_size as u64 * 4,
                    size: page_size * 3,
                },
            ],
        };

        let actual_memory_state = guest_memory.describe();
        assert_eq!(expected_memory_state, actual_memory_state);
    }

    #[test]
    fn test_dump() {
        let page_size = get_page_size().unwrap();

        // Two regions of two pages each, with a one page gap between them.
        let region_1_address = GuestAddress(0);
        let region_2_address = GuestAddress(page_size as u64 * 3);
        let region_size = page_size * 2;
        let mem_regions = [
            (region_1_address, region_size),
            (region_2_address, region_size),
        ];
        let guest_memory =
            kvmify(anonymous(mem_regions.into_iter(), true, HugePageConfig::None).unwrap());
        // Check that Firecracker bitmap is clean.
        guest_memory.iter().for_each(|r| {
            assert!(!r.bitmap().dirty_at(0));
            assert!(!r.bitmap().dirty_at(1));
        });

        // Fill the first region with 1s and the second with 2s.
        let first_region = vec![1u8; region_size];
        guest_memory.write(&first_region, region_1_address).unwrap();

        let second_region = vec![2u8; region_size];
        guest_memory
            .write(&second_region, region_2_address)
            .unwrap();

        let memory_state = guest_memory.describe();

        // dump the full memory.
        let mut memory_file = TempFile::new().unwrap().into_file();
        guest_memory.dump(&mut memory_file).unwrap();

        let restored_guest_memory =
            kvmify(file_private(memory_file, memory_state.regions(), false, 0).unwrap());

        // Check that the region contents are the same.
        let mut restored_region = vec![0u8; page_size * 2];
        restored_guest_memory
            .read(restored_region.as_mut_slice(), region_1_address)
            .unwrap();
        assert_eq!(first_region, restored_region);

        restored_guest_memory
            .read(restored_region.as_mut_slice(), region_2_address)
            .unwrap();
        assert_eq!(second_region, restored_region);
    }

    #[test]
    fn test_dump_dirty() {
        let page_size = get_page_size().unwrap();

        // Two regions of two pages each, with a one page gap between them.
        let region_1_address = GuestAddress(0);
        let region_2_address = GuestAddress(page_size as u64 * 3);
        let region_size = page_size * 2;
        let mem_regions = [
            (region_1_address, region_size),
            (region_2_address, region_size),
        ];
        let guest_memory =
            kvmify(anonymous(mem_regions.into_iter(), true, HugePageConfig::None).unwrap());
        // Check that Firecracker bitmap is clean.
        guest_memory.iter().for_each(|r| {
            assert!(!r.bitmap().dirty_at(0));
            assert!(!r.bitmap().dirty_at(1));
        });

        // Fill the first region with 1s and the second with 2s.
        let first_region = vec![1u8; region_size];
        guest_memory.write(&first_region, region_1_address).unwrap();

        let second_region = vec![2u8; region_size];
        guest_memory
            .write(&second_region, region_2_address)
            .unwrap();

        let memory_state = guest_memory.describe();

        // Dump only the dirty pages.
        // First region pages: [dirty, clean]
        // Second region pages: [clean, dirty]
        let mut dirty_bitmap: DirtyBitmap = HashMap::new();
        dirty_bitmap.insert(0, vec![0b01]);
        dirty_bitmap.insert(1, vec![0b10]);

        let mut file = TempFile::new().unwrap().into_file();
        guest_memory.dump_dirty(&mut file, &dirty_bitmap).unwrap();

        // We can restore from this because this is the first dirty dump.
        let restored_guest_memory =
            kvmify(file_private(file, memory_state.regions(), false, 0).unwrap());

        // Check that the region contents are the same.
        let mut restored_region = vec![0u8; region_size];
        restored_guest_memory
            .read(restored_region.as_mut_slice(), region_1_address)
            .unwrap();
        assert_eq!(first_region, restored_region);

        restored_guest_memory
            .read(restored_region.as_mut_slice(), region_2_address)
            .unwrap();
        assert_eq!(second_region, restored_region);

        // Dirty the memory and dump again
        let file = TempFile::new().unwrap();
        let mut reader = file.into_file();
        let zeros = vec![0u8; page_size];
        let ones = vec![1u8; page_size];
        let twos = vec![2u8; page_size];

        // Firecracker Bitmap
        // First region pages: [dirty, clean]
        // Second region pages: [clean, clean]
        guest_memory
            .write(&twos, GuestAddress(page_size as u64))
            .unwrap();

        guest_memory.dump_dirty(&mut reader, &dirty_bitmap).unwrap();

        // Check that only the dirty regions are dumped.
        let mut diff_file_content = Vec::new();
        let expected_first_region = [
            ones.as_slice(),
            twos.as_slice(),
            zeros.as_slice(),
            twos.as_slice(),
        ]
        .concat();
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_to_end(&mut diff_file_content).unwrap();
        assert_eq!(expected_first_region, diff_file_content);
    }

    #[test]
    fn test_store_dirty_bitmap() {
        let page_size = get_page_size().unwrap();

        // Two regions of three pages each, with a one page gap between them.
        let region_1_address = GuestAddress(0);
        let region_2_address = GuestAddress(page_size as u64 * 4);
        let region_size = page_size * 3;
        let mem_regions = [
            (region_1_address, region_size),
            (region_2_address, region_size),
        ];
        let guest_memory =
            kvmify(anonymous(mem_regions.into_iter(), true, HugePageConfig::None).unwrap());

        // Check that Firecracker bitmap is clean.
        guest_memory.iter().for_each(|r| {
            assert!(!r.bitmap().dirty_at(0));
            assert!(!r.bitmap().dirty_at(page_size));
            assert!(!r.bitmap().dirty_at(page_size * 2));
        });

        let mut dirty_bitmap: DirtyBitmap = HashMap::new();
        dirty_bitmap.insert(0, vec![0b101]);
        dirty_bitmap.insert(1, vec![0b101]);

        guest_memory.store_dirty_bitmap(&dirty_bitmap, page_size);

        // Assert that the bitmap now reports as being dirty maching the dirty bitmap
        guest_memory.iter().for_each(|r| {
            assert!(r.bitmap().dirty_at(0));
            assert!(!r.bitmap().dirty_at(page_size));
            assert!(r.bitmap().dirty_at(page_size * 2));
        });
    }

    #[test]
    fn test_create_memfd() {
        let size_bytes = mib_to_bytes(1) as u64;

        let memfd = create_memfd(size_bytes, None).unwrap();

        assert_eq!(memfd.as_file().metadata().unwrap().len(), size_bytes);
        memfd.as_file().set_len(0x69).unwrap_err();

        let mut seals = memfd::SealsHashSet::new();
        seals.insert(memfd::FileSeal::SealGrow);
        memfd.add_seals(&seals).unwrap_err();
    }

    #[test]
    fn test_bounce() {
        let file_direct = TempFile::new().unwrap();
        let file_bounced = TempFile::new().unwrap();
        let file_persistent_bounced = TempFile::new().unwrap();

        let mut data = (0..=255).collect_vec();

        MaybeBounce::new(file_direct.as_file().as_fd(), false)
            .write_all_volatile(&VolatileSlice::from(data.as_mut_slice()))
            .unwrap();
        MaybeBounce::new(file_bounced.as_file().as_fd(), true)
            .write_all_volatile(&VolatileSlice::from(data.as_mut_slice()))
            .unwrap();
        MaybeBounce::<_, 7>::new_persistent(file_persistent_bounced.as_file().as_fd(), true)
            .write_all_volatile(&VolatileSlice::from(data.as_mut_slice()))
            .unwrap();

        let mut data_direct = vec![0u8; 256];
        let mut data_bounced = vec![0u8; 256];
        let mut data_persistent_bounced = vec![0u8; 256];

        file_direct.as_file().seek(SeekFrom::Start(0)).unwrap();
        file_bounced.as_file().seek(SeekFrom::Start(0)).unwrap();
        file_persistent_bounced
            .as_file()
            .seek(SeekFrom::Start(0))
            .unwrap();

        MaybeBounce::new(file_direct.as_file().as_fd(), false)
            .read_exact_volatile(&mut VolatileSlice::from(data_direct.as_mut_slice()))
            .unwrap();
        MaybeBounce::new(file_bounced.as_file().as_fd(), true)
            .read_exact_volatile(&mut VolatileSlice::from(data_bounced.as_mut_slice()))
            .unwrap();
        MaybeBounce::<_, 7>::new_persistent(file_persistent_bounced.as_file().as_fd(), true)
            .read_exact_volatile(&mut VolatileSlice::from(
                data_persistent_bounced.as_mut_slice(),
            ))
            .unwrap();

        assert_eq!(data_direct, data_bounced);
        assert_eq!(data_direct, data);
        assert_eq!(data_persistent_bounced, data);
    }
}
