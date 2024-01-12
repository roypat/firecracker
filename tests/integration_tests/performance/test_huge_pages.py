# Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Integration tests for Firecracker's huge pages support"""
import pytest

from framework import utils
from framework.microvm import HugePagesConfig
from framework.properties import global_props


def check_hugetlbfs_in_use(pid):
    """Asserts that the process with the given pid is using hugetlbfs pages somewhere"""

    # Format of a smaps entry:
    #   7fc2bc400000-7fc2cc400000 rw-s 00000000 00:10 25488401                   /memfd:guest_mem (deleted)
    #   Size:             262144 kB
    #   KernelPageSize:     2048 kB
    #   MMUPageSize:        2048 kB
    #   Rss:                   0 kB
    #   Pss:                   0 kB
    #   Pss_Dirty:             0 kB
    #   Shared_Clean:          0 kB
    #   Shared_Dirty:          0 kB
    #   Private_Clean:         0 kB
    #   Private_Dirty:         0 kB
    #   Referenced:            0 kB
    #   Anonymous:             0 kB
    #   LazyFree:              0 kB
    #   AnonHugePages:         0 kB
    #   ShmemPmdMapped:        0 kB
    #   FilePmdMapped:         0 kB
    #   Shared_Hugetlb:        0 kB
    #   Private_Hugetlb:   92160 kB
    #   Swap:                  0 kB
    #   SwapPss:               0 kB
    #   Locked:                0 kB
    #   THPeligible:           0
    #   ProtectionKey:         0
    # the "memfd:guest_mem" is the identifier of our guest memory. It is memfd backed, with the memfd being called "guest_mem" in memory.rs
    cmd = f"cat /proc/{pid}/smaps | grep memfd:guest_mem -A 23 | grep KernelPageSize"
    _, stdout, _ = utils.run_cmd(cmd)

    kernel_page_size_kib = int(stdout.split()[1])
    assert kernel_page_size_kib > 4


@pytest.mark.skipif(
    global_props.host_linux_version == "4.14",
    reason="MFD_HUGETLB | MFD_ALLOW_SEALING only supported on kernels >= 4.16",
)
def test_hugetlbfs_boot(uvm_plain):
    """Tests booting a microvm with guest memory backed by 2MB hugetlbfs pages"""

    uvm_plain.spawn()
    uvm_plain.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB, mem_size_mib=128)
    uvm_plain.add_net_iface()
    uvm_plain.start()

    rc, _, _ = uvm_plain.ssh.run("true")
    assert not rc

    check_hugetlbfs_in_use(uvm_plain.firecracker_pid)
