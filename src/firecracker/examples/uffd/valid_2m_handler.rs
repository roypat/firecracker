// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provides functionality for a userspace page fault handler
//! which loads the whole region from the backing memory file
//! when a page fault occurs. Unlike valid_handler_4k, this handler faults in
//! huge pages, e.g. faults at 2M granularity (this is different than simply
//! pre-faulting 2M areas, as we need to align the faults to 2M boundaries to
//! support huge pages).

mod uffd_utils;

fn main() {
    uffd_utils::handle_faults(4096, 1024 * 1024 * 2) // 2MB
}
