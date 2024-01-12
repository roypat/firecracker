# Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Integration tests for Firecracker's huge pages support"""
import pytest

from framework import utils
from framework.microvm import HugePagesConfig
from framework.properties import global_props
from integration_tests.functional.test_uffd import SOCKET_PATH, spawn_pf_handler


def check_hugetlbfs_in_use(pid: int, allocation_name: str):
    """Asserts that the process with the given `pid` is using hugetlbfs pages somewhere.

    `allocation_name` should be the name of the smaps entry for which we want to verify that huge pages are used.
    For memfd-backed guest memory, this would be "memfd:guest_mem" (the `guest_mem` part originating from the name
    we give the memfd in memory.rs), for anonymous memory this would be "/anon_hugepage"
    """

    # Format of a sample smaps entry:
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
    cmd = f"cat /proc/{pid}/smaps | grep {allocation_name} -A 23 | grep KernelPageSize"
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

    check_hugetlbfs_in_use(uvm_plain.firecracker_pid, "memfd:guest_mem")


@pytest.mark.skipif(
    global_props.host_linux_version == "4.14",
    reason="MFD_HUGETLB | MFD_ALLOW_SEALING only supported on kernels >= 4.16",
)
def test_hugetlbfs_snapshot(
    microvm_factory, guest_kernel_linux_5_10, rootfs_ubuntu_22, uffd_handler_paths
):
    """
    Test hugetlbfs snapshot restore via uffd
    """

    ### Create Snapshot ###
    vm = microvm_factory.build(guest_kernel_linux_5_10, rootfs_ubuntu_22)
    vm.memory_monitor = None
    vm.spawn()
    vm.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB, mem_size_mib=128)
    vm.add_net_iface()
    vm.start()

    # Wait for microvm to boot
    rc, _, _ = vm.ssh.run("true")
    assert not rc

    check_hugetlbfs_in_use(vm.firecracker_pid, "memfd:guest_mem")

    snapshot = vm.snapshot_full()

    vm.kill()

    ### Restore Snapshot ###
    vm = microvm_factory.build()
    vm.spawn()

    # Spawn page fault handler process.
    _pf_handler = spawn_pf_handler(
        vm, uffd_handler_paths["valid_2m_handler"], snapshot.mem
    )

    vm.restore_from_snapshot(snapshot, resume=True, uffd_path=SOCKET_PATH)

    # Verify if guest can run commands.
    rc, _, _ = vm.ssh.run("true")
    assert not rc

    check_hugetlbfs_in_use(vm.firecracker_pid, "/anon_hugepage")
